# Davy design

Davy is one self-hosted binary that accepts what the Sentry SDKs and the Better Stack (Logtail) log shippers send. It groups errors into issues, stores logs, checks uptime, and serves all of it to people and to coding agents over MCP.

This document fixes the data shapes and the order of work.

## Principles

- **Wire compatibility over a new SDK.** An app switches by changing its DSN or source token. No code changes.
- **One process, one data directory.** SQLite holds metadata. Bulk data (event payloads, logs) sits behind a store interface, so the engine can change without touching the API.
- **Parse at the boundary.** HTTP handlers turn bytes into typed values (`Envelope`, `ErrorEvent`). Grouping and storage take typed values only and never see raw JSON shapes they have to guess at.
- **Lenient on fields, strict on framing.** A broken envelope or an invalid `event_id` is a 400. A field with an unexpected type or value (`level: "warn"`, a timestamp without a zone, a negative `lineno`) falls back to a default instead, because rejecting it would lose a customer's error.
- **Check the key before the work.** A request without a valid key costs Davy at most one header line of decompression. See Hardening.
- **Agent-first operation.** One binary with no database server, so a coding agent can install, connect and debug Davy from one prompt. Self-hosted Sentry runs 53 services and needs about 14 GB of RAM. Every step an operator takes is a non-interactive command with `--json` output and an error that names the next step. The docs are plain markdown an agent can fetch and follow, and their commands are tested.
- **Never fall over under a burst.** Ingest gets a bounded queue. When it fills, Davy answers 429 and the SDKs back off. One writer thread writes in batches.

## Milestone order

1. Storage benchmark (done, `bench/storage`, see "Log storage" below).
2. Errors: Sentry envelope and `/store/` ingest, grouping, issues, CLI to list them (done).
3. Logs: Logtail endpoint (msgpack and JSON), the Sentry `log` item, OTLP/HTTP logs, full-text search (done).
4. MCP and CLI for agents: `sql`, `schema`, `search_logs`, `issue_context`, `mark_fixed` (done).
5. **Source maps**: the `sentry-cli` upload endpoints and debug ID lookup (done).
6. Uptime checks and alerts: HTTP monitors, heartbeats, webhooks, Slack and e-mail (done).
7. **Install by prompt.** *Next.* A person pastes one prompt into a coding agent, and the agent installs Davy, connects the app and checks that errors arrive. See "Milestone 7" below.
8. **Release 0.1.0**: signed releases with SBOM and provenance, fuzzing, an internal penetration test. See "Milestone 8" below.
9. **Web UI for one team.** Its spec comes first.
10. Managed cloud (closed, see "License and open core"). Outside users come in only after the external penetration test in `SECURITY.md`.

How code reaches `main` is in `CONTRIBUTING.md`, how a version ships in `RELEASING.md`, and what security work runs when in `SECURITY.md`.

## Milestone 2: errors

### Wire protocol (what we accept)

Captured from real SDKs in `e2e/fixtures/` (`@sentry/node` 7.107.0 and 10.78.0, run `node e2e/capture.mjs <sdk>` to refresh).

- `POST /api/{project_id}/envelope/` with an envelope body.
- `POST /api/{project_id}/store/` with a single event JSON body (old SDKs such as `@sentry/react` 6).
- `OPTIONS` on both for browser CORS. Allow any origin and the headers `content-type`, `x-sentry-auth`, `sentry-trace`, `baggage`.
- The public key comes from, in order: the `X-Sentry-Auth` header (`Sentry sentry_key=..., sentry_version=7, ...`), the `sentry_key` query parameter (what `@sentry/node` uses, see the fixtures), or the `dsn` field of the envelope header.
- `Content-Encoding` may be `gzip`, `deflate`, `br` or `zstd`. Cap the compressed body at 20 MiB and the decompressed body at 200 MiB.
- Envelope format: a header JSON line, then for each item a header JSON line and a payload. If the item header has `length`, read exactly that many bytes, then skip one optional `\n`. Without `length`, the payload runs to the next `\n` or the end. A trailing newline is optional.

### Responses

- `200 {"id": "<event_id>"}` on success. `id` is the envelope header's `event_id`, or the first event's id, or a fresh one.
- Every response carries `X-Sentry-Rate-Limits: 86400:transaction;span;replay;profile;profile_chunk:organization` so SDKs stop sending data that milestone 2 drops. Bugsink does the same (`86400:transaction;span:organization`).
- `400` for an envelope we cannot parse, `401` for a missing key, `403` for a key that does not belong to the project.
- `413` when the body is over the cap.
- `429` with `Retry-After: 1` when the ingest queue is full.

### Data shapes

The organizing structure is a policy table from item type to what we do with it, and a state machine for issue status.

```rust
// boundary: parsed from bytes, never re-validated after this
struct Envelope { header: EnvelopeHeader, items: Vec<Item> }
struct EnvelopeHeader { event_id: Option<EventId>, dsn: Option<String>, sent_at: Option<String> }
struct Item { kind: ItemKind, payload: Bytes }

enum ItemKind { Event, Transaction, Span, Session, Sessions, ClientReport, Attachment, Log,
                ReplayEvent, ReplayRecording, Profile, ProfileChunk, CheckIn, Other(String) }

enum Disposition { StoreError, Drop(DataCategory) }   // one table: fn disposition(&ItemKind) -> Disposition
```

`Session`, `Sessions` and `ClientReport` are accepted and dropped in milestone 2 (release health comes later). `Log` is dropped until milestone 3. Unknown types are skipped, never an error.

```rust
struct ErrorEvent {
    event_id: EventId,
    timestamp: Timestamp,          // number (unix seconds) or RFC 3339 string on the wire
    received_at: Timestamp,
    platform: String,
    level: Level,                  // fatal | error | warning | info | debug, default error
    release: Option<String>,
    environment: Option<String>,
    message: Option<String>,       // `message` as string, or `logentry`/`message` object: prefer `formatted`, else `message`
    message_template: Option<String>, // `logentry.message` before params, used for grouping
    exceptions: Vec<Exception>,    // `exception.values` or legacy `exception` array; oldest first, last = the one raised
    fingerprint: Option<Vec<String>>,
    user_id: Option<String>,
    trace_id: Option<String>,      // contexts.trace.trace_id, 32 lowercase hex, else None
    raw: serde_json::Value,        // kept to store and serve the full payload
}
struct Exception { ty: String, value: Option<String>, module: Option<String>, frames: Vec<Frame> }
struct Frame { filename: Option<String>, abs_path: Option<String>, module: Option<String>,
               function: Option<String>, lineno: Option<u32>, colno: Option<u32>, in_app: Option<bool> }
```

`EventId` is 32 lowercase hex characters. Accept the dashed UUID form and normalize it.

### Grouping (pure function, versioned)

```rust
struct Grouping { version: u16, fingerprint: String /* hex of a 128-bit hash */, title: String, culprit: Option<String> }
fn group(e: &ErrorEvent) -> Grouping
```

Version 1 rules:

1. If the event has a `fingerprint`, hash its entries. An entry `{{ default }}` expands to the default components below.
2. Otherwise, with exceptions, for each exception in the chain take the type, plus the in-app frames (all frames if none is in-app). A frame contributes `module` (or the normalized filename) and `function`. Line and column numbers never contribute, so a deploy does not split issues. Take at most the 10 frames nearest the crash.
3. If an exception has no frames, use its type plus the normalized value.
4. Without exceptions, use `message_template`, else the normalized `message`.
5. Normalize: replace UUIDs, hex runs of 8 or more, numbers, email addresses and IPv4 addresses with placeholders. Strip a content hash from bundle filenames (`main.3f9a1c2b.js` becomes `main.js`) and query strings.

`title` is `"{type}: {value}"` for the last exception, else the message, truncated to 200 characters. `culprit` is the function of the frame nearest the crash among in-app frames.

A stored issue keeps its `grouping_version`. A later version 2 must not move old issues.

### Issue status

```rust
enum IssueStatus { Unresolved, Resolved { in_release: Option<String>, at: Timestamp }, Ignored }
```

A new event on a `Resolved` issue moves it back to `Unresolved` and sets `regressed_at`. An `Ignored` issue keeps counting but stays ignored.

### Storage (SQLite, WAL, one writer thread)

```sql
projects(id INTEGER PRIMARY KEY, slug TEXT UNIQUE NOT NULL, public_key TEXT UNIQUE NOT NULL, created_at INTEGER NOT NULL)
issues(id INTEGER PRIMARY KEY, project_id INTEGER NOT NULL, grouping_version INTEGER NOT NULL, fingerprint TEXT NOT NULL,
       title TEXT NOT NULL, culprit TEXT, level TEXT NOT NULL, status TEXT NOT NULL, resolved_in_release TEXT,
       resolved_at INTEGER, regressed_at INTEGER, first_seen INTEGER NOT NULL, last_seen INTEGER NOT NULL,
       event_count INTEGER NOT NULL, UNIQUE(project_id, grouping_version, fingerprint))
events(id INTEGER PRIMARY KEY, project_id INTEGER NOT NULL, issue_id INTEGER NOT NULL, event_id TEXT NOT NULL,
       timestamp INTEGER NOT NULL, received_at INTEGER NOT NULL, level TEXT NOT NULL, release TEXT, environment TEXT,
       trace_id TEXT, user_id TEXT, payload BLOB, UNIQUE(project_id, event_id))
issue_hourly(issue_id INTEGER NOT NULL, hour INTEGER NOT NULL, count INTEGER NOT NULL, PRIMARY KEY(issue_id, hour))
```

- Timestamps are unix milliseconds.
- `payload` is the zstd-compressed event JSON. Keep it for the first 50 events of an issue and then for every 100th. Always write the `events` row and bump the counters, so counts stay exact while disk stays bounded under a burst.
- A duplicate `event_id` in the same project is ignored (SDKs retry).
- The writer drains the queue in batches and commits each batch in one transaction.

### CLI

- `davy serve --data <dir> --listen <addr> --public-url <url>`
- `davy project create <slug> --data <dir> --public-url <url>` prints the DSN `http(s)://{public_key}@{host}/{project_id}`.
- `davy issues --data <dir> [--project <slug>] [--json]` lists issues, newest `last_seen` first, with id, status, count, title, culprit.
- `davy event <issue_id> --data <dir>` prints the latest stored payload as JSON.

### Done means

- Unit tests parse every fixture in `e2e/fixtures/` and assert the exact item kinds and the exception type and value.
- Grouping tests assert literal values: two events that differ only in a number or a UUID in the message land in one issue, and different exception types land in two.
- `e2e/run.sh` builds the binary, starts `davy serve` on a free port in a temp data dir under `.scratch/`, creates a project, runs a Node script against it with `@sentry/node` 7 and 10 (two captures of the same error and one different error per SDK), and asserts with `davy issues --json` that there are exactly the expected issues and counts. It stops the server at the end.

### Known follow-ups

- Decompression and JSON parsing run on the async runtime threads. Move them to `spawn_blocking` before load tests.
- Key lookup takes a `Mutex` around one SQLite connection. Cache the project keys in memory before load tests.
- A single bad event in an envelope fails the whole request with 400. Relay drops only the bad item. Decide when the first real case shows up.

## Milestone 3: logs

Three wire formats come in. One `LogRecord` shape goes out to one store. Each source is an adapter: a pure function from bytes to records, unit-tested on captured fixtures. The HTTP layer does auth, size caps and decompression the same way for all of them.

### Wire protocol (what we accept)

| Source | Route | Auth | Body | Success |
|---|---|---|---|---|
| Logtail (`@logtail/node`, `@logtail/pino`, Vector, Fluent Bit) | `POST /` | `Authorization: Bearer <public_key>` | msgpack, JSON or NDJSON | `202`, empty body |
| OTLP/HTTP logs | `POST /v1/logs` | `Authorization: Bearer <public_key>` | protobuf (`application/x-protobuf`) or JSON (`application/json`) | `200`, empty `ExportLogsServiceResponse` in the request's encoding and content type |
| Sentry `log` item | the existing envelope route | the existing Sentry key rules | `{"items":[...]}` inside an envelope | the existing `200 {"id":...}` |

- The bearer token is the project's `public_key`, so one DSN key also works as a Logtail source token and an OTLP header (`OTEL_EXPORTER_OTLP_HEADERS=Authorization=Bearer <key>`). The token alone picks the project.
- All three use the existing `Content-Encoding` handling and the 20 MiB / 200 MiB caps. The token is checked before decompression.
- Errors. `401` missing token, `403` unknown token, `413` too large, `429` with `Retry-After: 1` when the log queue is full. An undecodable Logtail body is `406` (what Better Stack answers). An undecodable OTLP body is `400` with a `google.rpc.Status` in the request's encoding. An OTLP content type other than protobuf or JSON is `415`.
- Lenient on fields here too. A record that is not an object is skipped. A field of the wrong type falls back to a default. A batch never fails because of one record.

### Data shape

```rust
pub struct LogRecord {
    pub project_id: i64,
    pub timestamp: i64,          // unix nanoseconds; logs need sub-millisecond order inside a burst
    pub observed_at: i64,        // unix nanoseconds, when Davy received it
    pub source: LogSource,       // Logtail | Otlp | Sentry
    pub level: LogLevel,         // Trace < Debug < Info < Warn < Error < Fatal, stored as 1..=6
    pub message: String,
    pub service: Option<String>,
    pub trace_id: Option<String>, // 32 lowercase hex, None when empty or all zeros
    pub span_id: Option<String>,  // 16 lowercase hex
    pub attributes: serde_json::Map<String, serde_json::Value>,
}
```

One level table for every source (`LogLevel::parse`). Logs default to `Info`, unlike error events.

| Input | Level |
|---|---|
| number (pino): ≤10, ≤20, ≤30, ≤40, ≤50, above | Trace, Debug, Info, Warn, Error, Fatal |
| OTLP `severity_number` 1-4, 5-8, 9-12, 13-16, 17-20, 21-24 | Trace, Debug, Info, Warn, Error, Fatal |
| string, case-insensitive: `trace`; `debug`; `info`, `information`, `notice`, `log`; `warn`, `warning`; `error`, `err`; `fatal`, `critical`, `crit`, `alert`, `emerg`, `emergency`, `panic` | in that order |
| OTLP `severity_number` 0 | parse `severity_text`, else Info |
| anything else | Info |

One timestamp function for every source. A number below 1e11 is seconds (fractions allowed), below 1e14 milliseconds, below 1e17 microseconds, else nanoseconds. Fractional numbers are rounded to the microsecond, because an f64 holds about 16 significant digits and the nanosecond digits of a seconds value are noise. A string is RFC 3339, with a naive `YYYY-MM-DDTHH:MM:SS[.f]` or a bare date read as UTC. A msgpack timestamp extension (type -1, 4, 8 or 12 bytes) is decoded per the msgpack spec, which allows at most 999,999,999 nanoseconds. `@logtail/pino` sends `dt` as an RFC 3339 string (captured fixture), but other msgpack shippers may encode a date natively. Anything else, or a time older than the retention window or more than 24 h in the future, becomes `observed_at`, and the original value is kept in the attribute `davy.original_timestamp`.

### Adapters

**Logtail.** Content type `application/msgpack` or `application/x-msgpack`, `application/json`, `application/x-ndjson` or `application/ndjson`. Without a content type, a body whose first non-space byte is `[` or `{` is JSON or NDJSON, else msgpack. The body is an array of records, one record, or one record per line. An NDJSON line that is JSON but not a record is skipped like any bad record. A msgpack body holds exactly one value, so bytes after it are a `406`. In each record `dt` is the timestamp, `message` the message (a non-string is written as compact JSON), `level` the level. `service`, `trace_id` and `span_id` are lifted out when they are strings, the ids only when they are valid hex. Every other key stays in `attributes` as it came, nested objects included. Decode msgpack with `rmpv`.

**OTLP.** Use the `opentelemetry-proto` message types (prost) for protobuf. Check its serde JSON against a real exporter capture first. OTLP JSON writes trace and span ids as hex, not base64, 64-bit integers as strings or numbers, and enums as integers. If the crate gets any of that wrong, parse the JSON by hand. Mapping:

- resource attribute `service.name` becomes `service`. Other resource attributes go into `attributes` as `resource.<key>`. A non-empty scope name goes in as `otel.scope.name`, a non-empty `event_name` as `otel.event_name`.
- the timestamp is `time_unix_nano`, else `observed_time_unix_nano`, else `observed_at`.
- a string `body` is the message. Any other body is written as compact JSON.
- `AnyValue` converts to JSON as string, bool, integer, float, array, object (from a kvlist), and base64 string (from bytes).

**Sentry `log` item.** `ItemKind::Log` moves from `Drop` to a new `Disposition::StoreLogs`. The item header has `item_count` and `content_type: application/vnd.sentry.items.log+json` but no `length`, so the payload runs to the end of its line. The payload is `{"version":2,"items":[{"timestamp": <float seconds>, "trace_id", "span_id"?, "level", "body", "severity_number"?, "attributes": {"<key>": {"value": ..., "type": ...}}}]}`. Attributes are flattened to `<key>: value`. `service.name`, when present, becomes `service`. An envelope with only log items still answers `200 {"id": ...}`. A log item that does not parse is skipped and the rest of the envelope is stored, as with other unknown items.

### Storage (Tantivy, one index per UTC day)

- `data/logs/YYYY-MM-DD/` is one Tantivy index for the UTC day of `timestamp`. Docstore compressed with zstd, as in the benchmark.
- Fields: `project` u64 indexed and fast. `ts` i64 indexed, fast and stored. `observed` i64 stored. `level` u64 indexed, fast and stored, so `--level warn` is a range query. `source` u64 stored. `service`, `trace_id` and `span_id` raw strings, indexed and stored. `message` text, stored. `attributes` a JSON field, indexed with the default tokenizer and stored, so `attributes.user_id:42` works.
- The JSON field stays. On 1M synthetic rows it took 0.48× the raw NDJSON size, under the 1.0× bar set before measuring, so the flat text variant was not measured. Numbers in `docs/storage-benchmark.md`, section "Logs ingest".
- One log writer thread owns the open `IndexWriter`s (at most 3 days open, least recently written one closed first, a day idle for 10 minutes closed). It reads batches of `LogRecord` from a queue bounded at 50k rows and 256 MiB of strings and attribute values, counted in records rather than batches because one batch holds one record or thousands and memory follows the records. Rows bound the fixed cost of a record. Bytes bound its strings, so 50k records of 1 MiB each cannot fill memory. An empty queue takes any batch, so one oversized batch is never refused forever. A full queue is the `429`. A bound of 256 batches let 1M rows in batches of 1000 push RSS to 828 MB (measured). Each writer uses 1 indexing thread and a 64 MB heap (1 thread was faster in the benchmark).
- The writer commits every second when anything changed, and at once when 100k documents are uncommitted. It commits everything on shutdown.
- A failed `add_document` or commit drops that day's `IndexWriter`, and the next write opens a fresh one. Tantivy does not restart its indexing worker after a failed commit, so the next 10k documents would block the writer thread forever and ingest would answer `429` from then on (reproduced in `store_recovers_when_a_day_index_breaks`). The cost is that the uncommitted documents of that day, at most about one second, are lost and logged.
- The HTTP handler answers once the batch is queued, not once it is committed. Error events wait for their commit because they are few and each matters. Logs come in bulk and the shippers retry on failure only. The cost is that a crash loses at most about one second of accepted logs, and a log shows up in search within about one second.
- Retention: `davy serve --log-retention-days <n>`, default 30, from 1 to 36,500 (100 years, past that the nanosecond cutoff overflows). Once an hour the writer renames day directories older than that into `data/logs/.trash/` and removes them. It touches only names that parse as `YYYY-MM-DD`.

### Search

```rust
pub struct LogQuery { pub project: Option<i64>, pub since: i64, pub until: i64,
                      pub min_level: Option<LogLevel>, pub text: Option<String>, pub limit: usize }
pub fn search(data: &Path, q: &LogQuery) -> anyhow::Result<Vec<LogHit>>   // newest first
```

- Opens only the day indexes that overlap `since..until`, newest first, sorts by the `ts` fast field and stops once it has `limit` hits.
- `text` goes through Tantivy's `QueryParser` with `message` and `attributes` as default fields. So `timeout`, `"connection reset"`, boolean queries such as `timeout AND db` and `attributes.user_id:42` all work. A parse error is shown to the user, not a panic.
- The CLI reads the indexes from its own process while the server writes. The writer's segment cleanup can delete a file just as the CLI opens it. Retry the search once on a missing-file error. The MCP server in milestone 4 runs inside the server process and avoids this.

### CLI

`davy logs --data <dir> [--project <slug>] [--since 1h|30m|2d|<RFC 3339>] [--until ...] [--level warn] [--limit 100] [--json] [query]`

- Defaults: `--since 24h`, `--limit 100`.
- Text output prints the newest `limit` hits oldest first, like `tail`: `2026-11-20T10:00:00.123Z WARN api message key=value ...`.
- `--json` prints one JSON object per line.

### Also in this milestone

Logs make the two load follow-ups from milestone 2 hot, so both land now.

- Project keys live in memory (`HashMap<public_key, project_id>` behind an `RwLock`) as a cache of known keys. A miss is one indexed SQLite lookup, and a hit is cached. A miss is not rate-limited, because a project created a moment ago must work at once and an unknown key costs one primary-key lookup. The Sentry routes use it too.
- Decompression and parsing run in `spawn_blocking` for every ingest route.

### Done means

- Fixtures captured from real clients with synthetic data into `e2e/fixtures/`, by extending `e2e/capture.mjs`. They are `logtail-pino` (`@logtail/pino`), `sentry-node-10-logs` (`@sentry/node` 10 with `enableLogs: true` and `Sentry.logger.*`), `otlp-proto` and `otlp-json` (the OpenTelemetry JS log exporters).
- Unit tests assert literal values. Each adapter parses its fixture into exact records. Every row of the level table and every timestamp form gets a case, the msgpack timestamp extension included. A Logtail batch with one bad record keeps the others.
- A store test writes three days of synthetic records and asserts exact hits and order for each of project, level, text, attribute query and time range. Retention then removes the oldest day.
- `e2e/run.sh` also sends logs through `@logtail/pino`, Sentry `logger.*` and both OTLP exporters, polls `davy logs` until all 10 records are searchable, and asserts with `davy logs --json` the exact message, level, service and one attribute per source, plus a trace id where the client sets one. A wrong bearer token gets 403.
- A load script sends 1M synthetic Logtail records in msgpack batches of 1000. Report rows per second and disk over raw size, with what limits them. It is a number to explain, not a gate.
- `cargo test` passes and `cargo clippy --all-targets -- -D warnings` is clean.

## Milestone 4: MCP and CLI for agents

A coding agent should get from "there is an error" to a fix without a human copying stack traces. It needs to read everything Davy stores, see one issue with everything around it, and say "fixed" in a way that reopens the issue if the error comes back after the fix ships.

### Transport and auth

- MCP over Streamable HTTP at `/mcp` inside `davy serve`, stateless, JSON responses (rmcp `legacy_session_mode = false`, `json_response = true`). In-process, so `mark_fixed` goes through the writer thread and log search never races segment cleanup. One port for ingest and agents. No stdio server: an agent on the same machine connects to `127.0.0.1`.
- `DAVY_TOKEN=<secret> davy serve`, at least 32 characters. There is no command line flag, because arguments show up in `ps` and shell history. `/mcp` takes `Authorization: Bearer <secret>`. Without a configured token `/mcp` is not mounted and answers 404. A project's ingest key is never accepted on `/mcp`, because it ships inside browser bundles.
- The token is compared as a blake3 hash, so the comparison takes the same time for every wrong token.
- Allowed `Host` values are loopback and the host of `--public-url`, against DNS rebinding.

### Tools

| Tool | Input | Output |
|---|---|---|
| `schema` | none | The `CREATE TABLE` statements of the SQLite tables, what the columns mean, and the log query syntax. |
| `sql` | `query` | `{columns, rows, truncated}`. One read-only statement. |
| `search_logs` | `query?`, `project?`, `since?`, `until?`, `level?`, `limit?` | Hits as `davy logs --json` prints them, oldest first. When more match than `limit`, the newest win. |
| `issue_context` | `issue_id` | The issue, its latest event made compact, its recent events and releases, and the logs around the latest event. |
| `mark_fixed` | `issue_id`, `release?` | A sentence that says which release the fix is expected in. |

**`sql`** opens its own read-only connection per call (`SQLITE_OPEN_READ_ONLY`, `PRAGMA query_only`), prepares exactly one statement, and runs it under an authorizer that allows only reads and function calls. `stmt.readonly()` is not enough, because SQLite counts `ATTACH` and `PRAGMA query_only=0` as read-only. At most 500 rows. A text cell over 4000 characters is cut and marked. A blob cell is `{"blob_bytes": n}`. The function `payload_json(payload)` decompresses an event payload to JSON text, so `json_extract(payload_json(payload), '$.request.url')` works. A query is interrupted after 5 s. A bad query is a tool error with SQLite's message, not a protocol error.

**`issue_context`** returns:

- `issue`: id, project slug, title, culprit, level, status, `event_count`, `first_seen`, `last_seen`, `resolved_in_release`, `regressed_at`. Times are RFC 3339.
- `latest_event`: from the newest stored payload. `event_id`, `timestamp`, `release`, `environment`, `platform`, `message`, `exceptions` (oldest first, each with type, value and at most the 30 frames nearest the crash; a frame keeps `filename`, `function`, `lineno`, `colno`, `in_app`, and for in-app frames `context_line`, `pre_context`, `post_context`), the last 20 `breadcrumbs`, `tags`, `user`, `request` (method, url), and the names and versions from `contexts.runtime`, `contexts.os` and `contexts.browser`. Everything else in the payload (SDK integrations, module lists, device memory) is dropped, because it costs tokens and rarely helps.
- `recent_events`: the last 10 events with `event_id`, `timestamp`, `release`, `environment`.
- `releases`: each release this issue was seen in, with its event count and last time.
- `logs`: with a trace id on the latest event, up to 50 logs of that trace within an hour of it. Otherwise up to 30 logs of the project from 60 s before to 10 s after it.

The compaction is a pure function from payload JSON to the `latest_event` value, tested on captured fixtures.

**`mark_fixed`** resolves the issue in a release:

- With `release`, the fix ships in that release.
- Without it, the fix ships in any release first seen after now.

### Regression rule

```rust
enum IssueStatus { Unresolved, Resolved { in_release: Option<String>, at: Timestamp }, Ignored }
impl IssueStatus { fn on_event(&mut self, release: Option<(&str, Timestamp /* release first seen */)>, at: Timestamp) -> Option<Timestamp> }
```

A new event on a `Resolved` issue reopens it (sets `regressed_at`) when:

- the event has no release, because Davy cannot tell whether it runs the fix, or
- its release is `in_release`, or
- its release was first seen after the issue was resolved.

An event from an older release still counts but does not reopen the issue, so a server that has not been redeployed yet does not undo the fix. `Ignored` keeps counting and stays ignored, as before.

A new table holds when each release was first seen:

```sql
releases(project_id INTEGER NOT NULL, version TEXT NOT NULL, first_seen INTEGER NOT NULL, PRIMARY KEY(project_id, version))
```

The writer inserts a row for each event with a release it has not seen. Schema changes from now on go through `PRAGMA user_version`. Version 1 creates `releases` and fills it from the existing `events` once, so an old data directory keeps the right first-seen times.

### Writer

The writer queue carries a job, either an ingest batch or a resolve. `mark_fixed` waits for the commit, like ingest. A resolve of an unknown issue is an error the tool returns to the agent.

### CLI

The same library functions back three commands, for agents that run shell commands instead of MCP:

- `davy sql --data <dir> <query>` prints a header and tab-separated rows, or with `--json` the same object the tool returns. `davy sql schema` prints what the `schema` tool returns.
- `davy context --data <dir> <issue_id>` prints `issue_context` as JSON.
- `davy resolve --data <dir> <issue_id> [--release <r>]` resolves from its own process. SQLite's locking orders it with the server's writer.

`--since` and `--until` parsing moves from `main.rs` into `time.rs`, so `search_logs` and `davy logs` read the same forms.

### Done means

- Unit tests with literal values: `on_event` for every row of the regression rule; `sql` returns exact rows, refuses a write, refuses two statements, cuts at 500 rows, decompresses with `payload_json`, and interrupts a runaway recursive query; compaction of the `sentry-node-10` error fixture gives the exact exception, frames and context lines.
- A store test resolves an issue in a release and asserts that an event from an older release keeps it resolved and one from a newer release reopens it.
- HTTP contract tests: `/mcp` is 401 without the token or with a wrong one and 403 for a `Host` it does not serve; with the right one `tools/list` and `tools/call` of `sql` and `mark_fixed` answer. Without a configured token `serve` does not merge the MCP router, so there is no route to test.
- `e2e/run.sh` connects with the official MCP TypeScript client (`@modelcontextprotocol/sdk`) over Streamable HTTP with the bearer token, lists the five tools, calls `issue_context` on an issue the Sentry SDKs created and asserts the exception type and an in-app frame, finds a log with `search_logs`, calls `mark_fixed` and reads the status back with `sql`.
- `scripts/check` passes.

## Milestone 5: source maps

A stack trace from minified JavaScript (`n` at `app.min.js:3:74`) is useless to a human and to an agent. With source maps uploaded, the same frame reads `parseOrder` at `src/app.js:5` with the source line. Davy takes the upload from the tool teams already run in CI, `sentry-cli`, and matches maps to events by debug ID only.

What `sentry-cli` 3.8.2 sends was captured with `e2e/capture-sourcemaps.mjs` into `e2e/fixtures/sentry-cli-3/`. `sentry-cli sourcemaps inject` writes a debug ID (a UUID derived from the content) into each minified file and its map. The SDK reports it per file in `debug_meta.images`.

### Wire protocol (what we accept)

All three routes take `Authorization: Bearer <token>`, the same token as `/mcp`, so `SENTRY_AUTH_TOKEN=$DAVY_TOKEN`. Without a configured token they are not mounted. The organization slug in the path is ignored. `SENTRY_URL` is the public URL.

- `GET /api/0/organizations/{org}/chunk-upload/` answers the upload options: `url` (this same route on the public URL), `chunkSize` 8 MiB, `chunksPerRequest` 4, `maxRequestSize` 32 MiB, `maxFileSize` 256 MiB, `concurrency` 1, `hashAlgorithm` `sha1`, `compression` `["gzip"]`, `accept` `["artifact_bundles"]`. Offering only artifact bundles keeps `sentry-cli` off the legacy release-file upload.
- `POST /api/0/organizations/{org}/chunk-upload/` is multipart. Each part is named `file` or `file_gzip` (gzip-compressed). Its filename is the SHA-1 of the uncompressed chunk. A chunk whose content does not hash to its name is a `400`. Body limit 33 MiB.
- `POST /api/0/organizations/{org}/artifactbundle/assemble/` takes `{"checksum", "chunks": [sha1], "projects": [slug], "version"?, "dist"?}`. If chunks are missing it answers `{"state": "not_found", "missingChunks": [...]}` and `sentry-cli` uploads them and asks again. Otherwise it joins the chunks in order, checks the SHA-1 of the whole against `checksum`, unpacks the bundle and answers `{"state": "created", "missingChunks": []}`. A bundle that does not unpack is `{"state": "error", "detail": "..."}`. Assembly runs in the request, so there is no `assembling` state.

The bundle is a zip. `manifest.json` maps each path to `{"type", "url", "headers": {"debug-id", "sourcemap"?}}`. Davy keeps the files of type `minified_source` and `source_map` that have a `debug-id` and ignores the rest. `projects` and `version` are not used for lookup: a debug ID is a content hash, so it names one build in every project.

### Storage

- `data/chunks/<sha1>` holds uploaded chunks until assembly. A successful assembly deletes its chunks.
- `data/sourcemaps/<debug-id>.map.zst` and `<debug-id>.js.zst` hold the map and the minified file, zstd-compressed. Files are written to a temporary name and renamed, so a reader never sees half a file and a repeated upload of the same build is harmless.
- These are files, not SQLite rows. The writer threads stay the only writers of SQLite and Tantivy.

### Symbolication

At ingest, before parsing into `ErrorEvent`, so the stored payload, grouping and `issue_context` all see original frames.

```rust
/// Rewrites minified frames in a Sentry event JSON in place. Pure apart from `maps`.
pub fn symbolicate(event: &mut serde_json::Value, maps: &impl Fn(DebugId) -> Option<Arc<Build>>)
```

- For each image in `debug_meta.images` with `type: "sourcemap"`, `code_file` names the minified file and `debug_id` the build.
- A frame whose `abs_path` or `filename` equals a `code_file` with a known build is looked up at (`lineno` − 1, `colno` − 1). The frame gets the original `filename` (the map's source), `lineno`, `colno`, the original function name, and `pre_context`, `context_line`, `post_context` (5 lines each) from `sourcesContent`. `in_app` becomes false when the source path contains `node_modules`. The minified position moves to `data.minified` (`{filename, lineno, colno, function}`), so nothing is lost.
- The original function name is the map's name for the nearest earlier token that spells the minified name right after the `function` keyword (`async function o` too), searched over at most 1024 tokens. Without a match, or without the minified file, the minified name stays.
- A frame with no image, no stored build or no token on its line stays as it is. A map without `sourcesContent` still gives file, line and function, without context lines.
- Parsed builds sit in a small cache (32 entries), so a burst of one error parses its map once.

Grouping is unchanged. It sees original frames when a map was uploaded before the event, so issues stay the same across builds. An event that arrived before its map stays minified.

### Not in this milestone

URL or release based lookup, the legacy release-file upload, fetching scripts from the web (it invites SSRF), Hermes and React Native maps, native debug files.

### Done means

- Unit tests with literal values: the captured event (`06.body`) symbolicated against the captured build gives `parseOrder` at `../src/app.js:5` with context line `  return order.items.length`, and `run` at line 11; a frame without a build, and a frame from another file, stay unchanged.
- HTTP contract tests replay the captured `sentry-cli` requests: every route is 401 without the token or with a wrong one; the options name the public URL; assemble answers `not_found` with the missing chunk, the chunk upload stores it, assemble then answers `created` and the stored build symbolicates a frame; a chunk renamed to another SHA-1 is 400; a fifth part, a chunk over 8 MiB unpacked and an assemble request for 33 chunks are 400; a bundle listing 10,001 files or unpacking past its byte limit does not unpack.
- `e2e/run.sh` runs `sentry-cli sourcemaps inject` and `upload` against the release binary, runs the bundle with `@sentry/node`, and asserts with `davy context` that the top in-app frame is `parseOrder` in `../src/app.js` at line 5.
- `scripts/check` passes.

## Milestone 6: uptime checks and alerts

Davy checks that a team's sites answer, notices when a cron job stops pinging, and tells the team when a monitor goes down or comes back and when an issue is new or regressed. Checks run from the one machine Davy runs on. Probes in more regions are a cloud feature.

Better Stack's uptime product is the reference (docs: Monitor types, Create a new monitor, Cron and heartbeat monitor): HTTP status, keyword and keyword absence, heartbeat with a grace period, a confirmation step before an incident opens. Sentry's issue alerts are the reference for issues (docs: Issue alert configuration): alert on a state change of an issue, never on every event.

### Data shapes

The organizing structures are a state machine per monitor and an outbox table that the writer fills in the same transaction as the change it reports. `Down` carries no incident ID, because the open incident is the one row of `incidents` with no `resolved_at` for the monitor.

```rust
struct Monitor { id: i64, project_id: i64, name: String, kind: Kind, every: Duration }

enum Kind {
    Http { url: CheckUrl, expect: Expect, timeout: Duration },    // CheckUrl: http or https, no user info
    Heartbeat { token: HeartbeatToken, grace: Duration },
}
// What `davy monitor add` asks for. The token is made when the row is stored.
enum Target { Http { url: CheckUrl, expect: Expect, timeout: Duration }, Heartbeat { grace: Duration } }
enum Expect { Status, Keyword(String), NoKeyword(String) }

enum Outcome { Up, Down(Reason) }                  // a check row keeps status and ms beside it
enum Reason { Timeout, Connect(String), Status(u16), KeywordMissing, KeywordFound, NotPublic, Late, Failed }

enum State { Pending, Up, Failing, Down }          // State::next(self, Outcome) -> (State, Option<Change>)
enum Change { Opened(Reason), Resolved }

// Holds what the body shows, read in the transaction of the change it reports.
enum Notice { IssueNew(Issue), IssueRegressed(Issue), MonitorDown(Incident), MonitorUp(Incident), Test }
struct Issue { id: i64, title: String, culprit: Option<String>, level: String, release: Option<String> }
struct Incident { id: i64, monitor: i64, name: String, reason: String }
enum Channel { Webhook { url: Url, secret: Secret }, Slack { url: Url } }
```

### Monitors

- An HTTP monitor sends `GET` to its URL every `every` (30 s to 24 h, default 60 s) with a `timeout` (1 to 60 s, default 10 s, shorter than `every`). It follows up to 5 redirects. It is up when the final status is below 400 and, with a keyword, when the first 1 MiB of the body contains the keyword (or, for `NoKeyword`, does not). Matching is case-sensitive, on the body as UTF-8 with invalid bytes replaced.
- A heartbeat monitor is up while pings arrive. It expects a ping every `every` (1 min to 31 d, default 1 h). It is down when no ping came within `every` plus `grace` (grace 0 to 24 h, default 5 min) of the last one, or when a ping goes to `/fail`. It is `Pending` until the first ping.
- A name is 1 to 100 characters, a keyword 1 to 1,000 bytes, a URL at most 2,048 bytes, `http` or `https`, with no user or password.
- **Confirmation.** One failed HTTP check moves `Up` to `Failing` and schedules the next check after 10 s instead of `every`. A second failure in a row opens an incident (`Down`). One success closes it. A heartbeat opens its incident on the first late check or `/fail`, because the grace period is its confirmation.
- One scheduler task in `davy serve` runs the checks, at most 16 at a time, the longest waiting first. Every second it reads which monitors are due, so `davy monitor add` in another process takes effect within a second and a restart resumes the schedule from `last_check_at`. A check is due `every` after the start of the last one, or 10 s after it while the monitor is `Failing`. A monitor stays busy until the writer has stored its result, so its checks never overlap.
- A check row keeps the status and `ms`, the time until the response headers arrived. The timeout covers reading the body too. A sixth redirect is a connection failure, and so is a connection error, whose text is cut to 200 characters and holds no URL.
- Check results and pings go to the writer queue. Every 10 s `davy serve` queues a sweep. The writer then opens an incident for each heartbeat that is up and late, and deletes up to 1,000 check rows older than 30 days. The check runs in the writer, so it cannot race a ping. A late heartbeat is noticed at most 10 s after its window ends. 1,000 rows every 10 s outpace the most a data directory adds, 1,000 monitors checking every 30 s.

### Heartbeat route

`GET`, `HEAD` or `POST /api/heartbeat/{token}` records a ping. `/api/heartbeat/{token}/fail` records a failure. The token is 32 lowercase hex characters made by `davy monitor add --heartbeat` and is the credential. A malformed or unknown token is `404`. The body is never read, so its size does not matter. A full writer queue is `429` with `Retry-After: 1`. The route waits for the writer, so `200` with body `ok` means the ping is stored.

### Reaching targets

HTTP checks and alert deliveries open connections to URLs a user typed. On a self-hosted server that user is the admin, so `davy serve` passes `Reach::Any` and any address is allowed, `localhost` included, because watching internal services is the point. The cloud crate passes `Reach::Public`, which allows only public addresses:

- IPv4 refuses every network the IANA special-purpose registry marks as not globally reachable: 0.0.0.0/8, 10.0.0.0/8, 100.64.0.0/10 (shared), 127.0.0.0/8, 169.254.0.0/16 (link-local, cloud metadata), 172.16.0.0/12, 192.0.0.0/24, the three documentation /24s, 192.168.0.0/16 and 198.18.0.0/15 (benchmarking). It also refuses 224.0.0.0/4 (multicast) and 240.0.0.0/4 (reserved and broadcast).
- An IPv6 address that carries an IPv4 address is judged by that IPv4 address: IPv4-mapped (::ffff:0:0/96), NAT64 (64:ff9b::/96) and 6to4 (2002::/16).
- Any other IPv6 address must be in 2000::/3, the global unicast block, and outside 2001::/23 (protocol assignments, Teredo included), 2001:db8::/32 and 3fff::/20 (documentation) and 5f00::/16 (SRv6). That refuses loopback, unspecified, unique local, link-local and multicast addresses without listing them.

A host name goes through a resolver that keeps only the public addresses DNS returns, and the connection goes to one of those, so a second DNS answer cannot swap in another address between check and connect. A name with no public address is refused. An IP address in a URL never reaches DNS, so it is checked before the request and on every redirect hop. A `Reach::Public` client uses no proxy, because a proxy would resolve the name itself. A refused check is the outcome `NotPublic`, and a refused delivery fails with `address is not public`.

### Alerts

- A channel belongs to one project or to all projects and receives every notice kind for them. Channels are a webhook, a Slack incoming webhook URL or an e-mail address.
- **Outbox.** When the writer opens a new issue, regresses one, opens or closes an incident, it inserts one `notifications` row per matching channel in the same transaction. A crash cannot lose a notice or send one for a change that did not commit. The body is rendered when the row is stored, so delivery only signs and sends it. A new issue is noticed once, by the event that created it, and a regression once, by the event that reopened it.
- **Delivery.** One notifier task looks for due rows every second and sends the oldest unsent row of each channel, one at a time per channel, so `monitor.down` never arrives after its `monitor.up`. A row waiting for its retry holds back the newer rows of its channel. A channel stays busy until the writer has stored the result, so one server never sends a row twice in a row. A send has 10 s, carries `User-Agent: Davy/<version>` and follows no redirect, so a receiver cannot bounce a signed body elsewhere and a `3xx` is a failure. A `2xx` marks the row sent. Anything else retries after 30 s, 1, 2, 4 and 8 min, then the row is marked failed and the channel moves on. `last_error` keeps the status (`HTTP 500`) or the connection error, cut to 200 characters and never with the URL, because the URL is a credential. An `https` URL is checked against the operating system's trust store, so a host without root certificates (a bare container image) needs its CA bundle installed. An invalid or expired certificate fails the send. Delivery is at least once, so a row in flight at shutdown is sent again on the next start. Each payload carries its `id` so a receiver can drop a repeat.
- **Bounds.** At most 100 unsent rows per channel. Over that a notice is not stored and the channel's `dropped` counter grows, which `davy channel list` shows.
- **Webhook.** `POST` with `content-type: application/json` and the headers `Davy-Delivery: <id>` and `Davy-Signature: t=<unix seconds>,v1=<hex HMAC-SHA256 of "<t>.<body>" with the channel secret>`. The secret is 32 random bytes as hex, printed once by `davy channel add`. The HMAC key is those 64 characters as text, not the decoded bytes. The scheme matches Stripe's, so receivers can reuse their verification code.

```json
{"id": 18, "type": "issue.new", "project": "shop", "at": "2026-11-20T08:42:01Z",
 "issue": {"id": 3, "title": "TypeError: x is undefined", "culprit": "parseOrder(src/app.js)", "level": "error", "release": "shop@2.0.0"}}
{"id": 19, "type": "monitor.down", "project": "shop", "at": "2026-11-20T08:43:10Z",
 "monitor": {"id": 1, "name": "API"}, "incident": {"id": 4, "reason": "HTTP 503"}}
```

- **Slack.** `POST {"text": "<line>"}` to the incoming webhook URL, for example `[shop] API is down: HTTP 503`. The lines are `New issue: <title>`, `Issue regressed: <title>`, `<monitor> is down: <reason>`, `<monitor> is up again` and `Test notice from Davy`, after `[<project>]` when the notice has one. Titles and reasons are cut to 200 characters. `&`, `<` and `>` are escaped, so an event title cannot mention `@channel` or hide a link. A Slack line carries no `id`, because Slack cannot drop a repeat.
- **E-mail.** `davy serve` sends through the SMTP server in `DAVY_SMTP_URL`, read only from the environment because it holds the password, from the address in `--mail-from`. Either one without the other stops the start. `smtps://user:pass@host:465` speaks TLS from the first byte. `smtp://user:pass@host:587?tls=required` upgrades with STARTTLS and fails when the server cannot. `smtp://host:25` is plain, for a relay on the same host or network. A URL with a password in any other form is refused at start, `tls=opportunistic` included, because an attacker on the path can strip STARTTLS and read the password. The certificate is checked against the operating system's trust store, like `https`. The SMTP server is the operator's choice, so `Reach` does not apply to it.
- One message per row, plain UTF-8 text. The subject is the Slack line without the escaping, with control characters replaced by spaces, so a title cannot add a header. The body is the line, a blank line, then the details as `name: value` lines, as below. An issue has `title`, `issue_id`, `level` and, when set, `culprit` and `release`. The `Message-ID` is `<davy.<row id>.<created ms>@<domain of --mail-from>>`, so a repeat after a restart has the same ID and a mail client can drop it. A send has 10 s. Any SMTP error, a refused recipient included, retries on the schedule above, and `last_error` keeps the server's answer cut to 200 characters. Without `DAVY_SMTP_URL` an e-mail row fails with `no SMTP server, set DAVY_SMTP_URL` and retries, so `davy channel list` shows the problem. A cloud tenant could mail any address, so the cloud crate must verify a recipient before it sends.

```
Subject: [shop] API is down: HTTP 503

[shop] API is down: HTTP 503

monitor: API
monitor_id: 1
incident_id: 4
reason: HTTP 503
at: 2026-11-20T08:43:10Z
```

- `type` is one of `issue.new`, `issue.regressed`, `monitor.down`, `monitor.up`, `test`.

### Storage

New tables, added by migration, so existing data directories keep opening:

- `monitors(id, project_id, name, kind, url, expect, keyword, heartbeat_token UNIQUE, every_s, timeout_s, grace_s, state, last_check_at, last_ping_at, created_at)`. `state` is `Failing` after one failed check, so no failure counter is needed.
- `checks(monitor_id, at, ok, status, ms, reason)` with an index on `(monitor_id, at)`
- `incidents(id, monitor_id, started_at, resolved_at, reason)`
- `channels(id, project_id NULL for all, kind, url, secret, dropped, created_at)`
- `notifications(id, channel_id, type, payload, created_at, attempts, next_at, sent_at, failed_at, last_error)` with an index on `(channel_id, id) WHERE sent_at IS NULL AND failed_at IS NULL`

At most 1,000 monitors and 100 channels per data directory. The `sql` tool reads `monitors.heartbeat_token`, `channels.url` and `channels.secret` as `NULL`, because an agent has no use for a credential and a prompt-injected agent could leak it.

### CLI

- `davy monitor add <project> <name> --url <url> [--keyword <text> | --no-keyword <text>] [--every 60s] [--timeout 10s]`
- `davy monitor add <project> <name> --heartbeat [--every 1h] [--grace 5m]` prints the ping path `/api/heartbeat/<token>`.
- `davy monitor list` shows each monitor with its state, last check and open incident. `davy monitor remove <id>`.
- `davy channel add webhook <url> (--project <slug> | --all)` prints the channel ID and the signing secret, once. `davy channel add slack <url> (--project <slug> | --all)` and `davy channel add email <address> (--project <slug> | --all)` print the ID. An address is one `user@domain` of at most 254 characters, stored as a `mailto:` URL. The project is a flag because clap cannot read an optional positional before two required ones.
- `davy channel list` shows each channel with its unsent, failed and dropped counts and where it goes: only the host of a webhook or Slack URL, because a Slack webhook URL is a credential, and the whole address of an e-mail channel. `davy channel remove <id>`. `davy channel test <id>` queues a `test` notice for that channel alone. Its `project` is `null` on a channel of every project.

Monitors and channels are added from the CLI only. No MCP tool creates them, so an agent cannot point the server at an address.

### Not in this milestone

Status pages, regions and remote probes, ping, TCP, DNS and TLS expiry checks, custom request headers and bodies, escalation policies, on-call schedules, SMS and calls, alerts on escalating issues and on log queries, silences and maintenance windows.

### Done means

- Unit tests with literal values: the state machine for every pair of state and outcome; `Reach::Public` for a table of addresses, IPv4-mapped, NAT64 and 6to4 included; check outcomes against a local test server for status, keyword, keyword absence, timeout, a redirect to a refused address and a body over 1 MiB; the webhook signature of a fixed body, time and secret; the retry schedule; the outbox order per channel; the heartbeat route with a known, an unknown and a `/fail` token; the `sql` tool reading `NULL` for the three credential columns.
- `e2e/alerts.mjs`, run by `e2e/run.sh`, starts its own `davy serve` with a local site, a local webhook and Slack receiver and a local SMTP server, and one channel of each kind for all projects. The site answers 503 until the monitor is down, then 200. `@sentry/node` sends two new issues. A heartbeat gets a ping, a failure ping and a ping, then misses its window. Each channel must get the same 7 notices: both `issue.new`, `monitor.down` then `monitor.up` for the site, and `monitor.down`, `monitor.up`, `monitor.down` for the heartbeat. Every webhook signature must verify with the channel's secret, every e-mail must have its own `Message-ID`, and `davy channel list` must show nothing unsent or failed. It takes about 75 s, most of it waiting for the missed heartbeat.
- `scripts/check` passes.

## Milestone 7: install by prompt

Davy's edge in self-hosting is that there is nothing to run next to it. This milestone turns that into a setup a coding agent finishes alone: install the binary, run it as a service, connect the app's Sentry SDK and log shipper, connect itself over MCP, and prove that an error arrives.

### The prompt

The README opens with one prompt to paste into Claude Code, Codex or Cursor:

> Install Davy on this machine and connect this app to it. Follow `<docs URL>/install.md`.

The docs URL is the repository's raw `docs/` on the release tag until Davy has its own domain.

### Docs written for agents

- `docs/install.md` is a list of steps. Each step has one command, what it prints on success, and what to do when it fails. No step needs a browser, a terminal prompt or a GUI.
- The agent asks the person only for what it cannot know: the public URL and whether TLS is already in front of it, and optionally an SMTP server for e-mail alerts.
- `docs/connect/` has one page per client: Node, the browser with source maps through `sentry-cli`, Python, Go, Logtail and pino, and OpenTelemetry. Each uses the official SDK and changes only the DSN or token.
- `llms.txt` at the root of the repository lists these pages with one line each.
- Every command in a fenced `sh` block of these pages runs in e2e (Done means), so a page cannot fall behind the binary.

### Commands for agents

- `davy setup --data DIR --public-url URL [--listen ADDR] [--mail-from ADDR] [--service systemd|launchd|none]` prepares a machine. It creates the data directory readable only by its owner, writes a random admin token to `DIR/token` with mode 0600 when none exists, records its flags in `DIR/setup.json` for `doctor` and `mcp-config`, writes the service file into `DIR` and prints JSON: the paths it wrote and the next commands. Running it twice changes nothing and prints the same JSON.
- Setup runs as the user the service runs as and never needs root. It refuses to write a systemd unit for root, because the server parses untrusted input, and it refuses a directory that holds files and none of Davy's, because it makes the directory private. The commands that need root (installing the unit, `systemctl`) are printed, not run, so the person sees each one.
- The systemd unit runs the server with write access to the data directory only (`ProtectSystem=strict`, `ReadWritePaths`), no capabilities, no new privileges and a private `/tmp`. The token reaches it as a path in `DAVY_TOKEN_FILE`. The SMTP URL, which holds a password, reaches it the same way through `DIR/smtp-url` and `DAVY_SMTP_URL_FILE`.
- `davy serve` also reads the admin token from the file named by `DAVY_TOKEN_FILE`, so a service file holds a path, not the token.
- `davy doctor --data DIR [--json]` checks a running install. Each check has a status (`ok`, `warn`, `fail`) and, unless it is `ok`, the command or setting that fixes it. It checks the data directory's permissions, the token's presence and length, that the server answers at the public URL, that the public URL is `https` unless it is a loopback address, that the SMTP server accepts a connection when one is set, and free disk space. It sends nothing to the internet beyond the public URL and the SMTP server. The exit code is 1 when a check fails.
- `davy mcp-config --client claude-code|codex|cursor` prints how to add Davy to that client. The token is referenced through an environment variable or the token file where the client supports it, and is never printed without `--show-token`.
- Every CLI error says what to run or set next.

### Safety

The agent runs these commands with the person's shell rights. The docs never tell it to put the token on a command line, to disable TLS checks, to widen file permissions or to pipe a download into a shell. The install step downloads the binary, checks it against `SHA256SUMS` and, when `gh` is present, against its build provenance, and only then moves it into place.

### Done means

- Unit tests with literal values: `setup` on an empty and on a prepared directory, the service files it writes, a `doctor` table where each broken install (open permissions, short token, server down, plain `http` public URL, refused SMTP) reports its own check and fix, and `mcp-config` for each client.
- `e2e/install.sh` starts a clean Debian container with only the binary, extracts the `sh` blocks from `docs/install.md` and `docs/connect/node.md` and runs them in order. `davy doctor --json` must report every check `ok`, `@sentry/node` sends an error, and the MCP `issue_context` tool returns it.
- An agent eval: Claude Code and Codex, each in a fresh container with a small Node app and nothing but the prompt, finish with the app's test error visible over MCP. Five runs each. The pass rate and the median time go into this section, and a failed run becomes a fix in the docs or the CLI.
- `scripts/check` passes.

## Milestone 8: release 0.1.0

The first version someone can download, verify and run without building it. `RELEASING.md` and the security program in `SECURITY.md` are the spec. This milestone builds what they mark as planned.

### Done means

- `scripts/release X.Y.Z` cuts a release as `RELEASING.md` describes, and refuses to run on a dirty tree, off `main`, behind `origin/main` or with an empty `## Unreleased`.
- A test tag runs the release workflow end to end. Its artifacts verify with `gh attestation verify`, `sha256sum -c SHA256SUMS` and `cosign verify`, and `cargo audit bin` reads the dependency list from a downloaded binary.
- The release workflow passes `zizmor`, pins every action to a SHA, and only its publish job can write.
- A fuzz target exists for every parser in `SECURITY.md`, each ran for at least an hour without a crash, and CI runs each for a few minutes every night.
- The internal penetration test report exists, and every critical and high finding is fixed with a test.
- The owner's commits and the release tag are signed and show as verified on GitHub.
- The e2e suite passes against the downloaded release binary and the container image.

## Hardening

A security review of `16f413c` showed that a client without a key could make the server allocate 4.4 GB with one 194 KB request (measured with a release build on a developer laptop: a gzip body that inflates to 190 MiB of `[0,0,...]` sent to `/store/`). The rules below close that class. Each has a test that a request over the limit is refused.

### Authentication before work

- A Sentry request with the key in `X-Sentry-Auth` or `?sentry_key=` is checked before the body is read.
- `/store/` has no envelope header, so a `/store/` request without a header or query key is a `401` before the body is read.
- An envelope without a header or query key is decompressed only up to the end of its first line, at most 64 KiB. Davy parses that line as the envelope header and checks the key in its `dsn` against the project in the path. Only then does it decompress the rest. A header line over 64 KiB is a `400`.
- Logtail and OTLP check the bearer token before reading the body, as before.

### Sizes and counts

Sentry's own limits (develop.sentry.dev, Envelopes, Size Limits) are 200 MiB for a decompressed envelope and 1 MiB for an event or log item. Davy uses the same numbers, so an SDK that works with Sentry works with Davy.

| Input | Limit | Over the limit |
|---|---|---|
| Request body, compressed | 20 MiB | `413` |
| Request body, decompressed | 200 MiB | `413` |
| Event item, and a `/store/` body | 1 MiB | the item is dropped, like any dropped item type |
| Log item in an envelope | 1 MiB | the item is dropped |
| Event items per envelope | 1 (develop.sentry.dev, Envelope Items) | later event items are dropped |
| Frames symbolicated per event | 256, from the frame that threw outward | older frames stay minified |
| Log message | 64 KiB | cut at a character boundary, attribute `davy.truncated` set to `true` |
| Log queue | 50,000 rows or 256 MiB of strings and attribute values | `429` |
| `payload_json()` output in `sql` | 16 MiB | the query fails with an error |
| Files in an artifact bundle | 10,000, and 1 GiB unpacked in total | assemble answers `error` |
| Parts in a chunk upload | 4, the announced `chunksPerRequest` | `400` |
| Chunk, decompressed | 8 MiB, the announced `chunkSize` | `400` |
| Chunks in an assemble request | 32, a 256 MiB bundle in 8 MiB chunks | `400` |

### Time and concurrency

- At most 8 requests decode and parse a body at the same time. One more is a `429` with `Retry-After: 1`, so a burst of large bodies cannot hold more than 8 decoded bodies in memory.
- Reading a request body has 30 seconds. A slower client gets `408`. Slow headers are the reverse proxy's job, because SECURITY.md already requires one for TLS.

### Other inputs

- A `trace_id` that is not 32 hex characters is ignored, so it cannot break the log query in `issue_context`.
- The CLI replaces control characters other than newline and tab in titles, messages and attributes with `U+FFFD` before printing to a terminal. `--json` output is unchanged, because JSON already escapes them.
- The admin token is read only from `DAVY_TOKEN`. A token on the command line shows up in `ps` and shell history.
- The SMTP URL is read only from `DAVY_SMTP_URL`, for the same reason, because it holds the SMTP password.

### Before the cloud launch

These are fine for one team on its own server and must change before tenants share one:

- **Source maps per tenant.** Builds are stored by debug ID only. One tenant could upload a map under another tenant's public debug ID and rewrite its stack traces. The store key and the lookup gain the project.
- **Retention for errors.** Events, issues and hourly counts are kept forever. A sender with a key can open a new issue per event. A retention sweep by age and a cap on new issues per project per hour come with the cloud quotas.
- **Abandoned chunks.** Chunks of an upload that never assembles stay in `data/chunks/`. A sweep removes chunks older than an hour.
- **Tenant scope in agent tools.** `sql` and `mark_fixed` see every project in the database.
- **E-mail recipients.** An e-mail channel mails any address. The cloud verifies an address with a confirmation link before a channel sends to it.

## License and open core

- **Open, AGPL-3.0 (this repo).** The single binary someone runs themselves: Sentry, Logtail and OTLP ingest, issues, log search, uptime, alerts by e-mail, webhook and Slack, CLI, MCP, and a basic UI for one team.
- **Closed, in a private `davy-cloud` repo that depends on this crate.** The managed cloud: what it takes to run Davy for many teams at once.
- Contributions need a CLA, so the core can also be sold under a commercial license.
- Keep the core usable as a library: the cloud must not need a fork.

## Log storage (decided by benchmark)

Logs go into Tantivy, one index per day, documents stored zstd-compressed. At 10M rows it took 0.67× the raw size on disk against 1.80× for SQLite FTS5, and counted by level in 4.2 ms against 98 ms. Numbers, method and tradeoffs are in `docs/storage-benchmark.md`.
