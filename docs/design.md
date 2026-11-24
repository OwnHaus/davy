# Davy design

Davy is one self-hosted binary that accepts what the Sentry SDKs and the Better Stack (Logtail) log shippers send. It groups errors into issues, stores logs, checks uptime, and serves all of it to people and to coding agents over MCP.

This document fixes the data shapes and the order of work.

## Principles

- **Wire compatibility over a new SDK.** An app switches by changing its DSN or source token. No code changes.
- **One process, one data directory.** SQLite holds metadata. Bulk data (event payloads, logs) sits behind a store interface, so the engine can change without touching the API.
- **Parse at the boundary.** HTTP handlers turn bytes into typed values (`Envelope`, `ErrorEvent`). Grouping and storage take typed values only and never see raw JSON shapes they have to guess at.
- **Lenient on fields, strict on framing.** A broken envelope or an invalid `event_id` is a 400. A field with an unexpected type or value (`level: "warn"`, a timestamp without a zone, a negative `lineno`) falls back to a default instead, because rejecting it would lose a customer's error.
- **Check the key before the work.** A request without a valid key costs Davy at most 128 KiB of body and one header line of decompression. See Hardening.
- **Agent-first operation.** One binary with no database server, so a coding agent can install, connect and debug Davy from one prompt. Self-hosted Sentry runs 53 services and needs about 14 GB of RAM. Every step an operator takes is a non-interactive command with `--json` output and an error that names the next step. The docs are plain markdown an agent can fetch and follow, and their commands are tested.
- **Never fall over under a burst.** Ingest gets a bounded queue. When it fills, Davy answers 429 and the SDKs back off. One writer thread writes in batches.

## Milestone order

1. Storage benchmark (done, `bench/storage`, see "Log storage" below).
2. Errors: Sentry envelope and `/store/` ingest, grouping, issues, CLI to list them (done).
3. Logs: Logtail endpoint (msgpack and JSON), the Sentry `log` item, OTLP/HTTP logs, full-text search (done).
4. MCP and CLI for agents: `sql`, `schema`, `search_logs`, `issue_context`, `mark_fixed` (done), `submit_feedback` (milestone 7).
5. **Source maps**: the `sentry-cli` upload endpoints and debug ID lookup (done).
6. Uptime checks and alerts: HTTP monitors, heartbeats, webhooks, Slack and e-mail (done).
7. **Install by prompt** (done). A person pastes one prompt into a coding agent, and the agent installs Davy, connects the app and checks that errors arrive. See "Milestone 7" below.
8. **Release 0.1.0**: signed releases with SBOM and provenance, fuzzing, an internal penetration test (done). See "Milestone 8" below.
9. **Web UI for one team** (done). See "Milestone 9" below.
10. Managed cloud (closed, see "License and open core"). Outside users come in only after the external penetration test in `SECURITY.md`.

How code reaches `main` is in `CONTRIBUTING.md`, how a version ships in `RELEASING.md`, and what security work runs when in `SECURITY.md`.

## Milestone 2: errors

### Wire protocol (what we accept)

Captured from real SDKs in `e2e/fixtures/` (`@sentry/node` 7.107.0 and 10.78.0, run `node e2e/capture.mjs <sdk>` to refresh).

- `POST /api/{project_id}/envelope/` with an envelope body.
- `POST /api/{project_id}/store/` with a single event JSON body (old SDKs such as `@sentry/react` 6).
- `OPTIONS` on both for browser CORS. Allow any origin and the headers `content-type`, `x-sentry-auth`, `sentry-trace`, `baggage`.
- The public key comes from, in order: the `X-Sentry-Auth` header (`Sentry sentry_key=..., sentry_version=7, ...`), the `sentry_key` query parameter (what `@sentry/node` uses, see the fixtures), or the `dsn` field of the envelope header.
- `Content-Encoding` may be `gzip`, `deflate`, `br` or `zstd`. Cap the compressed body at 20 MiB and the decompressed body at 64 MiB. A zstd frame may ask for a window of at most 8 MiB, the most RFC 9659 requires of an HTTP decoder. Large-window brotli, which RFC 7932 does not define, is a `400`.
- Envelope format: a header JSON line, then for each item a header JSON line and a payload. If the item header has `length`, read exactly that many bytes, then skip one optional `\n`. Without `length`, the payload runs to the next `\n` or the end. A trailing newline is optional. Both header lines must be JSON objects. Davy reads `event_id` and `dsn` from the envelope header and `type` and `length` from an item header, and skips other fields without building them.

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
struct Envelope<'a> { header: EnvelopeHeader, items: Vec<Item<'a>> }
struct EnvelopeHeader { event_id: Option<EventId>, dsn: Option<String> }
struct Item<'a> { kind: ItemKind, payload: &'a [u8] }   // borrows the decoded body

enum ItemKind { Event, Transaction, Span, Session, Sessions, ClientReport, Attachment, Log,
                ReplayEvent, ReplayRecording, Profile, ProfileChunk, CheckIn, Feedback, UserReport, Other }

enum Disposition { StoreError, StoreLogs, SdkItem, Drop }   // one table: fn disposition(&ItemKind) -> Disposition
```

`SdkItem` is every small item Davy keeps in SQLite through one writer job: `ClientReport`, `Feedback`, `UserReport`, `Session` and `Sessions`. `Log` was dropped until milestone 3. The `SdkItem` kinds were dropped in 0.1.0 and are kept since, see [What else the SDKs send](#what-else-the-sdks-send). Unknown types are skipped, never an error.

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
struct Exception { ty: String, value: Option<String>, frames: Vec<Frame> }
struct Frame { filename: Option<String>, abs_path: Option<String>, module: Option<String>,
               function: Option<String>, in_app: Option<bool> }
```

`EventId` is 32 lowercase hex characters. Accept the dashed UUID form and normalize it.

### Grouping (pure function, versioned)

```rust
struct Grouping { version: u16, fingerprint: String /* hex of a 128-bit hash */, title: String, culprit: Option<String> }
fn group(e: &ErrorEvent) -> Grouping
```

Version 1 rules:

1. If the event has a `fingerprint`, hash its entries. An entry `{{ default }}` expands to the default components below, once, because a repeat adds nothing and would cost the fingerprint's length times the default's size.
2. Otherwise, with exceptions, for each exception in the chain take the type, plus the in-app frames (all frames if none is in-app). A frame contributes `module` (or the normalized filename) and `function`. Line and column numbers never contribute, so a deploy does not split issues. Take at most the 10 frames nearest the crash.
3. If an exception has no frames, use its type plus the normalized value.
4. Without exceptions, use `message_template`, else the normalized `message`.
5. Normalize: replace UUIDs, hex runs of 8 or more, numbers, email addresses and IPv4 addresses with placeholders. Strip a content hash from bundle filenames (`main.3f9a1c2b.js` becomes `main.js`) and query strings.

`title` is `"{type}: {value}"` for the last exception, or `{type}` when it has no value, else the message, truncated to 200 characters. `culprit` is the function of the frame nearest the crash among in-app frames.

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
issue_tags(key TEXT NOT NULL, value TEXT NOT NULL, issue_id INTEGER NOT NULL, PRIMARY KEY(key, value, issue_id)) WITHOUT ROWID
issues_text USING fts5(title, culprit, content='issues', tokenize='trigram')
```

`issue_tags` and `issues_text` serve the filters of the web UI's issue list. Triggers keep both in step. Milestone 9's "Load check" says why.

- Timestamps are unix milliseconds.
- `payload` is the zstd-compressed event JSON. Keep it for the first 50 events of an issue, then for every 100th, and for the newest, so an issue always shows its latest event. The newest drops its payload when a newer event arrives, unless the sample keeps it. A payload kept only for being the newest starts with an empty zstd skippable frame, `5D 2A 4D 18 00 00 00 00`, which zstd decoders skip, and only a payload with that mark is dropped. The sample cannot be worked out from `event_count` afterwards, because expired events lower it. Payloads stored before the mark keep theirs, at most one per issue. The writer compresses the newest event of each issue once per transaction, not every event, so a burst costs no more than before. Always write the `events` row and bump the counters, so counts stay exact while disk stays bounded under a burst.
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
- All three use the existing `Content-Encoding` handling and the 20 MiB / 64 MiB caps and the memory budget. The token is checked before decompression.
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

One timestamp function for every source. A number below 1e11 is seconds (fractions allowed), below 1e14 milliseconds, below 1e17 microseconds, else nanoseconds. Fractional numbers are rounded to the microsecond, because an f64 holds about 16 significant digits and the nanosecond digits of a seconds value are noise. A string is RFC 3339, with a naive `YYYY-MM-DDTHH:MM:SS[.f]` or a bare date read as UTC. A msgpack timestamp extension (type -1, 4, 8 or 12 bytes) is decoded per the msgpack spec, which allows at most 999,999,999 nanoseconds. `@logtail/pino` sends `dt` as an RFC 3339 string (captured fixture), but other msgpack shippers may encode a date natively. Anything else, or a time older than its project's log retention or more than 24 h in the future, becomes `observed_at`, and the original value is kept in the attribute `davy.original_timestamp`.

### Adapters

**Logtail.** Content type `application/msgpack` or `application/x-msgpack`, `application/json`, `application/x-ndjson` or `application/ndjson`. Without a content type, a body whose first non-space byte is `[` or `{` is JSON or NDJSON, else msgpack. The body is an array of records, one record, or one record per line. An NDJSON line that is JSON but not a record is skipped like any bad record. A msgpack body holds exactly one value, so bytes after it are a `406`, and it nests at most 129 levels, as JSON does. In each record `dt` is the timestamp, `message` the message (a non-string is written as compact JSON), `level` the level. `service`, `trace_id` and `span_id` are lifted out when they are strings, the ids only when they are valid hex. Every other key stays in `attributes` as it came, nested objects included. Decode msgpack with `rmpv`.

**OTLP.** Use the `opentelemetry-proto` message types (prost) for protobuf. Check its serde JSON against a real exporter capture first. OTLP JSON writes trace and span ids as hex, not base64, 64-bit integers as strings or numbers, and enums as integers. If the crate gets any of that wrong, parse the JSON by hand. Mapping:

- resource attribute `service.name` becomes `service`. Other resource attributes go into `attributes` as `resource.<key>`. A non-empty scope name goes in as `otel.scope.name`, a non-empty `event_name` as `otel.event_name`.
- the timestamp is `time_unix_nano`, else `observed_time_unix_nano`, else `observed_at`.
- a string `body` is the message. Any other body is written as compact JSON.
- `AnyValue` converts to JSON as string, bool, integer, float, array, object (from a kvlist), and base64 string (from bytes).

**Sentry `log` item.** `ItemKind::Log` moves from `Drop` to a new `Disposition::StoreLogs`. The item header has `item_count` and `content_type: application/vnd.sentry.items.log+json` but no `length`, so the payload runs to the end of its line. The payload is `{"version":2,"items":[{"timestamp": <float seconds>, "trace_id", "span_id"?, "level", "body", "severity_number"?, "attributes": {"<key>": {"value": ..., "type": ...}}}]}`. Attributes are flattened to `<key>: value`. `service.name`, when present, becomes `service`. An envelope with only log items still answers `200 {"id": ...}`. A log item that does not parse is skipped and the rest of the envelope is stored, as with other unknown items.

### Storage (Tantivy, one index per UTC day)

- `data/logs/YYYY-MM-DD/` is one Tantivy index for the UTC day of `timestamp`. Docstore compressed with zstd, as in the benchmark.
- Fields: `project` u64 indexed and fast. `ts` i64 indexed, fast and stored. `observed` i64 stored. `level` u64 indexed, fast and stored, so `--level warn` is a range query. `source` u64 stored. `service`, `trace_id` and `span_id` raw strings, indexed and stored. `message` text, stored. `attributes` a JSON field, indexed with the default tokenizer and stored, so `attributes.user_id:42` works. `values` text, not stored, holds every string and number in `attributes` at any depth. A bare word in a JSON default field matches only the root path, which no attribute has, so without `values` a request ID in `attributes.req.id` was found only by its path. Day indexes created before `values` existed keep their schema, and the writer and search look the field up per day.
- The JSON field stays. On 1M synthetic rows it took 0.48× the raw NDJSON size, under the 1.0× bar set before measuring, so the flat text variant was not measured. Numbers in `docs/storage-benchmark.md`, section "Logs ingest".
- One log writer thread owns the open `IndexWriter`s (at most 3 days open, least recently written one closed first, a day idle for 10 minutes closed). It reads batches of `LogRecord` from a queue bounded at 50k rows and 128 MiB of heap, counted in records rather than batches because one batch holds one record or thousands and memory follows the records. A record's heap is priced by `heap::of_record`: the record, its text and its attribute tree with the allocator's slack, so 50k records of 1 MiB each, or of a thousand empty attributes each, cannot fill memory. A batch holds its share from the moment it is queued until the writer has indexed it, not only while it waits. A full queue is the `429`. A batch larger than the whole queue is `413`, because it would never fit. A bound of 256 batches let 1M rows in batches of 1000 push RSS to 828 MB (measured). Each writer uses 1 indexing thread and a 64 MB heap (1 thread was faster in the benchmark).
- The writer commits every second when anything changed, and at once when 100k documents are uncommitted. It commits everything on shutdown.
- A failed `add_document` or commit drops that day's `IndexWriter`, and the next write opens a fresh one. Tantivy does not restart its indexing worker after a failed commit, so the next 10k documents would block the writer thread forever and ingest would answer `429` from then on (reproduced in `store_recovers_when_a_day_index_breaks`). The cost is that the uncommitted documents of that day, at most about one second, are lost and logged.
- The HTTP handler answers once the batch is queued, not once it is committed. Error events wait for their commit because they are few and each matters. Logs come in bulk and the shippers retry on failure only. The cost is that a crash loses at most about one second of accepted logs, and a log shows up in search within about one second.
- Retention: `davy serve --log-retention-days <n>`, default 30, from 1 to 36,500 (100 years, past that the nanosecond cutoff overflows), and a project can keep its logs for a number of days of its own. At start and every minute the writer renames day directories older than every retention into `data/logs/.trash/` and removes them. It touches only real directories whose name is the canonical `YYYY-MM-DD` of a date, never a symlink, and empties `.trash` only when it is a real directory, and only of real directories named `YYYY-MM-DD-<digits>`, the names it gives them. Anything else there stays and is reported on stderr. "Retention per project" says how a project with a shorter or longer retention is cut out of a day.

### Search

```rust
pub struct LogQuery { pub project: Option<i64>, pub since: i64, pub until: i64,
                      pub min_level: Option<LogLevel>, pub text: Option<String>, pub limit: usize }
pub fn search(data: &Path, q: &LogQuery) -> anyhow::Result<Vec<LogHit>>   // newest first
```

- Opens only the day indexes that overlap `since..until`, newest first, sorts by the `ts` fast field and stops once it has `limit` hits.
- Keeps each day it opened open for the next search (`logs/days.rs`). Opening a day costs about 1 ms, so a rare needle over 90 days took 86 to 101 ms before and 2.3 to 6.0 ms after (measured, `docs/storage-benchmark.md`). A day is reopened when the size, mtime or inode of its `meta.json` changed, because every commit replaces that file, so only today's day reopens. At most 400 days stay open, least recently searched closed first, so the memory maps stay well under Linux's 65,530 per process. Each segment caches 8 decompressed doc store blocks instead of Tantivy's 100. Retention drops a day from the cache before it moves the directory to the trash.
- `text` goes through Tantivy's `QueryParser` with `message` and `values` as default fields, or `message` and `attributes` in a day without `values`. So `timeout`, `r-3fa9c2d1`, `"connection reset"`, boolean queries such as `timeout AND db` and `attributes.user_id:42` all work. A parse error is shown to the user, not a panic.
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
- Allowed `Host` values are loopback and the host of `--public-url`, against DNS rebinding. A request with an `Origin` must come from loopback or the public URL, so another site's page gets `403` even before the token is checked.

### Tools

| Tool | Input | Output |
|---|---|---|
| `schema` | none | The `CREATE TABLE` statements of the SQLite tables, what the columns mean, and the log query syntax. |
| `sql` | `query` | `{columns, rows, truncated}`. One read-only statement. |
| `search_logs` | `query?`, `project?`, `since?`, `until?`, `level?`, `limit?` | Hits as `davy logs --json` prints them, oldest first. When more match than `limit`, the newest win. |
| `count_logs` | `query?`, `project?`, `since?`, `until?`, `level?` | `{total, step, buckets}` as `davy logs --count --json` prints it. At most 200 buckets of a round width, and only those with logs, as `[start, count]`. |
| `connect_project` | `slug`, `sdk?` | Existing project DSN, SDK instructions and test command. New projects require local CLI access. |
| `project_status` | `slug` | Event count, last event time (ms), issue count, last log time (ns) and `retention`: the days of logs and errors the project keeps. |
| `set_retention` | `project`, `logs_days?`, `errors_days?` | What the project keeps now, as `retention` in `project_status`. A setting is 1 to 36,500 days or `"default"`, and may only keep data longer. |
| `release_health` | `project?`, `since?` | Per release and environment, newest first, at most 50: `sessions`, `errored`, `crashed`, `abnormal`, `crash_free_sessions`, `users`, `crashed_users`, `crash_free_users`, `first_hour`, `last_hour`. `since` defaults to `7d`. |
| `issue_context` | `issue_id` | The issue, its latest event made compact, its recent events and releases, and the logs around the latest event. |
| `mark_fixed` | `issue_id`, `release?` | A sentence that says which release the fix is expected in. |
| `submit_feedback` | `tried`, `happened`, `fix?` | A sentence that says the feedback is stored. |

**`sql`** opens its own read-only connection per call (`SQLITE_OPEN_READ_ONLY`, `PRAGMA query_only`), prepares exactly one statement, and runs it under an authorizer that allows only reads and function calls. `stmt.readonly()` is not enough, because SQLite counts `ATTACH` and `PRAGMA query_only=0` as read-only. At most 500 rows. A text cell over 4000 characters is cut and ends in `…[cut]`. A string or blob over 16 MiB is an error, so one query cannot take a gigabyte. A blob cell is `{"blob_bytes": n}`. The function `payload_json(payload)` decompresses an event payload to JSON text, so `json_extract(payload_json(payload), '$.request.url')` works. A query is interrupted after 5 s. A bad query is a tool error with SQLite's message, not a protocol error.

**`issue_context`** returns:

- `issue`: id, project slug, title, culprit, level, status, `event_count`, `first_seen`, `last_seen`, `resolved_in_release`, `regressed_at`. Times are RFC 3339.
- `latest_event`: from the newest stored payload. `event_id`, `timestamp`, `release`, `environment`, `platform`, `message`, `exceptions` (oldest first, each with type, value and at most the 30 frames nearest the crash; a frame keeps `filename`, `function`, `lineno`, `colno`, `in_app`, and for in-app frames `context_line`, `pre_context`, `post_context`), the last 20 `breadcrumbs`, `tags`, `user`, `request` (method, url), and the names and versions from `contexts.runtime`, `contexts.os` and `contexts.browser`. Everything else in the payload (SDK integrations, module lists, device memory) is dropped, because it costs tokens and rarely helps.
- `recent_events`: the last 10 events with `event_id`, `timestamp`, `release`, `environment`.
- `releases`: each release this issue was seen in, with its event count and last time.
- `logs`: with a trace id on the latest event, up to 50 logs of that trace within an hour of it. Otherwise up to 30 logs of the project from 60 s before to 10 s after it.
- `feedback`: the newest 10 things users wrote about the issue's events, each with `received_at`, `message`, `name`, `email`, `url` and `event_id`.

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

The writer inserts a row for each event with a release it has not seen. Schema changes from now on go through `PRAGMA user_version`. A store whose version is above the migrations a binary knows was migrated by a newer Davy, and is refused before anything changes, with a message that names that release and the backup taken before the upgrade: an older binary ignores the tables and columns it does not know, and with per-project retention that deletes data. Builds from before this check opened such a store. Version 1 creates `releases` and fills it from the existing `events` once, so an old data directory keeps the right first-seen times.

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
- HTTP contract tests: `/mcp` is 401 without the token or with a wrong one and 403 for a `Host` or `Origin` it does not serve; with the right one `tools/list` and `tools/call` of `sql` and `mark_fixed` answer. Without a configured token `serve` does not merge the MCP router, so there is no route to test.
- `e2e/run.sh` connects with the official MCP TypeScript client (`@modelcontextprotocol/sdk`) over Streamable HTTP with the bearer token, lists the eleven tools, calls `issue_context` on an issue the Sentry SDKs created and asserts the exception type and an in-app frame, finds a log with `search_logs` and counts it with `count_logs`, calls `mark_fixed` and reads the status back with `sql`, and lengthens a project's retention with `set_retention` and sees a shorter one refused.
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
- A frame whose `abs_path` or `filename` equals a `code_file` with a known build is looked up at (`lineno` − 1, `colno` − 1). The frame gets the original `filename` (the map's source), `lineno`, `colno`, the original function name, and `pre_context`, `context_line`, `post_context` (5 lines each) from `sourcesContent`. A line over 256 bytes keeps the 256 around the original column, with `…` at each cut, because a source that is itself minified has lines of megabytes. `in_app` becomes false when the source path contains `node_modules`. The minified position moves to `data.minified` (`{filename, lineno, colno, function}`), so nothing is lost.
- The original function name is the map's name for the nearest earlier token that spells the minified name right after the `function` keyword (`async function o` too), searched over at most 1024 tokens. Without a match, or without the minified file, the minified name stays.
- A frame with no image, no stored build or no token on its line stays as it is. A map without `sourcesContent` still gives file, line and function, without context lines.
- Parsed builds sit in a cache of at most 32 builds and 256 MiB, and the oldest go first. A build is priced from its files before it parses (`src/sourcemaps/price.rs`), because a map of a few megabytes can describe millions of tokens and lines: what the parse holds at once, and what the build keeps with every line of every file indexed. Measured on a real 1.77 MB map: the parse peaked at 10 MB and the build kept 8.4 MB, priced at 20 MB and 11 MB. A map or minified file whose parse would take more than 256 MiB fails assemble, so `sentry-cli` shows the error. A build whose map and minified file together cost more is not parsed. A build keeps less than its parse took, so every build that parses fits in the cache. Index and Hermes maps, which Davy cannot read, also fail assemble. One map parses at a time, and requests that need a build being parsed wait for it, so a burst of one error after a restart parses its map once. An event waits for parses 5 s in total and looks up the builds of at most 8 debug IDs, those its newest frames name. Frames it gives up on stay minified. Anyone with a project key picks which uploaded builds an event names, so without these limits events that cycle through more builds than the cache holds would hold many parsed builds each and queue every other event behind their parses. The event pays the budget for each frame symbolication rewrites before the next one, so it holds at most one frame it has not paid for.

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
// Issue notices of one channel whose window ended together, oldest first.
// `id` and `at` are the oldest row's.
struct Digest { id: i64, at: Timestamp, entries: Vec<Entry> }
struct Entry { regressed: bool, project: String, issue: Issue }
enum Channel { Webhook { url: Url, secret: Secret }, Slack { url: Url } }
```

### Monitors

- An HTTP monitor sends `GET` to its URL every `every` (30 s to 24 h, default 60 s) with a `timeout` (1 to 60 s, default 10 s, shorter than `every`). It follows up to 5 redirects. It is up when the final status is below 400 and, with a keyword, when the first 1 MiB of the body contains the keyword (or, for `NoKeyword`, does not). Matching is case-sensitive, on the body as UTF-8 with invalid bytes replaced.
- A heartbeat monitor is up while pings arrive. It expects a ping every `every` (1 min to 31 d, default 1 h). It is down when no ping came within `every` plus `grace` (grace 0 to 24 h, default 5 min) of the last one, or when a ping goes to `/fail`. It is `Pending` until the first ping.
- A name is 1 to 100 characters, a keyword 1 to 1,000 bytes, a URL at most 2,048 bytes, `http` or `https`, with no user or password.
- **Confirmation.** One failed HTTP check moves `Up` to `Failing` and schedules the next check after 10 s instead of `every`. A second failure in a row opens an incident (`Down`). One success closes it. A heartbeat opens its incident on the first late check or `/fail`, because the grace period is its confirmation.
- One scheduler task in `davy serve` runs the checks, at most 16 at a time, the longest waiting first. Every second it reads which monitors are due, so `davy monitor add` in another process takes effect within a second and a restart resumes the schedule from `last_check_at`. A check is due `every` after the start of the last one, or 10 s after it while the monitor is `Failing`. A monitor stays busy until the writer has stored its result, so its checks never overlap.
- A check row keeps the status and `ms`, the time until the response headers arrived. The timeout covers reading the body too. A sixth redirect is a connection failure, and so is a connection error, whose text is cut to 200 characters and holds no URL.
- Check results and pings go to the writer queue. Every 10 s `davy serve` queues a sweep. The writer then opens an incident for each heartbeat that is up and late, and deletes check rows older than 30 days, 1,000 at a time, until none are left or the sweep has spent 100 ms deleting. The check runs in the writer, so it cannot race a ping. A late heartbeat is noticed at most 10 s after its window ends. Even one batch every 10 s outpaces the most `davy monitor add` lets a data directory add, 1,000 monitors checking every 30 s. The rest of the budget is for monitors made by check-ins, which that limit does not cover ("Bounds" under "Cron check-ins").

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
- **Outbox.** When the writer opens a new issue, regresses one, opens or closes an incident, it inserts one `notifications` row per matching channel in the same transaction. A crash cannot lose a notice or send one for a change that did not commit. The body is rendered when the row is stored, so delivery only signs and sends it. A digest is the exception: it is rendered from its rows when they are due. A new issue is noticed once, by the event that created it, and a regression once, by the event that reopened it.
- **Delivery.** One notifier task looks for due rows every second and sends the oldest unsent alert of each channel, or its oldest feedback when no alert waits, one at a time per channel, so `monitor.down` never arrives after its `monitor.up`. A row waiting for its retry holds back the newer alerts of its channel. Feedback never holds back an alert, and issue rows waiting for their window hold back nothing. A channel stays busy until the writer has stored the result, so one server never sends a row twice in a row. A send has 10 s, carries `User-Agent: Davy/<version>` and follows no redirect, so a receiver cannot bounce a signed body elsewhere and a `3xx` is a failure. A `2xx` marks the row sent. Anything else retries after 30 s, 1, 2, 4 and 8 min, then the row is marked failed and the channel moves on. `last_error` keeps the status (`HTTP 500`) or the connection error, cut to 200 characters and never with the URL, because the URL is a credential. An `https` URL is checked against the operating system's trust store, so a host without root certificates (a bare container image) needs its CA bundle installed. An invalid or expired certificate fails the send. Delivery is at least once, so a row in flight at shutdown is sent again on the next start. Each payload carries its `id` so a receiver can drop a repeat.
- **Digest.** An issue notice (`issue.new` or `issue.regressed`) waits up to 30 s, so the issues of a broken deploy or of a batch of test events reach a channel as one message, not one each. The first issue notice of a channel opens a window of 30 s, and every issue notice for that channel until it ends joins it, from any project the channel covers. The window does not grow, so a long burst sends one message every 30 s. When it ends, one waiting notice goes out with the body it would have alone, and two or more go out as one digest. CLI monitor, HTTP monitor, feedback and test notices never wait and pass the waiting issue notices. Each notice keeps its own row, with its project and issue in the row's `issue` column, so the window survives a restart. A digest's rows are marked together: a failed digest retries as a whole, with the same rows, `id` and `Message-ID`, and the notices of a later window wait behind it. The window is a constant: 30 s holds the burst of one deploy or one test run and still tells a team about a new issue within a minute.
- **SDK monitor digest.** SDK monitor transitions use a separate 100-row outbox budget per channel. Excess SDK transitions increment the channel's `sdk_dropped` counter, not `dropped`, because anyone with the DSN can drive it. They never consume the 100 places for operator monitor and issue alerts. The first SDK transition waits 30 seconds; later transitions join that window. Delivery groups ready SDK rows into one `monitor.digest` message, even for one row. Every attempt, including a failed attempt, starts a persisted 30-second channel cooldown. Operator alerts and due issue digests take priority, so SDK retries cannot hold them back. The resulting SDK send rate is at most two attempts per minute per channel, across all projects it covers. The tests drive 100 flapping monitors, webhook, Slack and email channels, and assert that CLI alerts go out immediately and issues at 30 seconds. A webhook receives `{"id":21,"type":"monitor.digest","notices":[...]}`. Each entry in `notices` has the existing `monitor.down` or `monitor.up` webhook shape, including its own id, time and project. Existing types retain their payloads. Slack and email receive the number of changes and a pointer to `/ui/monitors`. A retry includes ready rows at that attempt; a receiver can deduplicate individual entries by id. This is a bounded notification summary, not an audit stream; incidents remain the source of uptime history.
- A digest links each issue to its page in the web UI, `<public URL>/ui/issues/<id>`. A webhook gets every issue of the digest, because a receiver may act on each one. The bound on unsent rows keeps that to 100. `id` and `at` are those of the oldest row, and `project` is set when every issue is of one project:

```json
{"id": 21, "type": "issue.digest", "project": null, "at": "2026-11-20T08:42:01Z",
 "issues": [{"type": "issue.new", "project": "shop", "url": "https://davy.example.com/ui/issues/3",
             "issue": {"id": 3, "title": "TypeError: x is undefined", "culprit": "parseOrder(src/app.js)", "level": "error", "release": "shop@2.0.0"}},
            {"type": "issue.regressed", "project": "blog", "url": "https://davy.example.com/ui/issues/5",
             "issue": {"id": 5, "title": "RangeError: y", "culprit": null, "level": "error", "release": null}}]}
```

- Slack and e-mail get a line such as `12 new issues in 5 projects`, or `[shop] 2 new and 1 regressed issues` when the issues share a project, then the first 5 issues, each as its notice line and its link, then `and 7 more: <public URL>/ui/issues`. With titles cut to 200 characters a Slack digest stays far below the 40,000 characters after which Slack cuts a message. The e-mail subject is the first line, and the `Message-ID` that of the oldest row.

```
Subject: 1 new and 1 regressed issues in 2 projects

1 new and 1 regressed issues in 2 projects

[shop] New issue: TypeError: x is undefined
https://davy.example.com/ui/issues/3

[blog] Issue regressed: RangeError: y
https://davy.example.com/ui/issues/5
```

- **Bounds.** At most 100 unsent alerts per channel in each of three classes: issue notices, notices of monitors made by check-ins, and the operator's own monitor and test notices. Anyone with a project's public DSN can make new issues and SDK monitors, so a flood of either cannot push out a CLI or HTTP monitor notice. Over that a notice is not stored and a counter of the channel grows: `sdk_dropped` for a notice of a monitor made by check-ins, `dropped` for any other. A DSN holder can raise `sdk_dropped` at will, so `dropped` above 0 always means an issue, feedback or operator notice was lost. `davy channel list`, its `--json`, the Alerts page and the `sql` tool show both. Feedback from agents has 5 places of its own on top, so a flood of feedback cannot push out an alert.
- **Webhook.** `POST` with `content-type: application/json` and the headers `Davy-Delivery: <id>` and `Davy-Signature: t=<unix seconds>,v1=<hex HMAC-SHA256 of "<t>.<body>" with the channel secret>`. The secret is 32 random bytes as hex, printed once by `davy channel add`. The HMAC key is those 64 characters as text, not the decoded bytes. The scheme matches Stripe's, so receivers can reuse their verification code.

```json
{"id": 18, "type": "issue.new", "project": "shop", "at": "2026-11-20T08:42:01Z",
 "issue": {"id": 3, "title": "TypeError: x is undefined", "culprit": "parseOrder(src/app.js)", "level": "error", "release": "shop@2.0.0"}}
{"id": 19, "type": "monitor.down", "project": "shop", "at": "2026-11-20T08:43:10Z",
 "monitor": {"id": 1, "name": "API"}, "incident": {"id": 4, "reason": "HTTP 503"}}
```

- **Slack.** `POST {"text": "<line>", "unfurl_links": false, "unfurl_media": false}` to the incoming webhook URL, for example `[shop] API is down: HTTP 503`. The lines are `New issue: <title>`, `Issue regressed: <title>`, `<monitor> is down: <reason>`, `<monitor> is up again` and `Test notice from Davy`, after `[<project>]` when the notice has one. Titles and reasons are cut to 200 characters. `&`, `<` and `>` are escaped, so an event title cannot mention `@channel` or hide a link. Unfurling is off, so Slack does not fetch a URL that an event title carries. A Slack line carries no `id`, because Slack cannot drop a repeat.
- **E-mail.** `davy serve` sends through the SMTP server in `DAVY_SMTP_URL`, `DAVY_SMTP_URL_FILE` or `DIR/smtp-url`, never from a flag because it holds the password, from the address in `--mail-from` or `DIR/setup.json`. Either one without the other stops the start. `smtps://user:pass@host:465` speaks TLS from the first byte. `smtp://user:pass@host:587?tls=required` upgrades with STARTTLS and fails when the server cannot. `smtp://host:25` is plain, for a relay on the same host or network. A URL with a password in any other form is refused at start, `tls=opportunistic` included, because an attacker on the path can strip STARTTLS and read the password. The certificate is checked against the operating system's trust store, like `https`. The SMTP server is the operator's choice, so `Reach` does not apply to it.
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

- `type` is one of `issue.new`, `issue.regressed`, `issue.digest`, `monitor.down`, `monitor.up`, `feedback`, `test`. A `feedback` notice goes only to channels added with `--feedback`, has `project: null` and a `feedback` object with `id`, `client`, `tried`, `happened` and `fix`. Its Slack line is `Feedback from <client>: <happened>`.

### Storage

New tables, added by migration, so existing data directories keep opening:

- `monitors(id, project_id, name, kind, url, expect, keyword, heartbeat_token UNIQUE, every_s, timeout_s, grace_s, state, last_check_at, last_ping_at, created_at)`. `state` is `Failing` after one failed check, so no failure counter is needed.
- `checks(monitor_id, at, ok, status, ms, reason)` with an index on `(monitor_id, at)`
- `incidents(id, monitor_id, started_at, resolved_at, reason)`
- `channels(id, project_id NULL for all, kind, url, secret, dropped, sdk_dropped, created_at)`
- `notifications(id, channel_id, type, payload, created_at, attempts, next_at, sent_at, failed_at, last_error, issue)` with an index on `(channel_id, id) WHERE sent_at IS NULL AND failed_at IS NULL`

At most 1,000 monitors and 100 channels per data directory. The `sql` tool reads `monitors.heartbeat_token`, `monitors.url`, `channels.url` and `channels.secret` as `NULL`, and since the web UI `users.password`, `sessions.hash` and `invites.hash` too, because an agent has no use for a credential and a prompt-injected agent could leak it. A monitor URL counts, because its query string often holds a token.

### CLI

- `davy monitor add <project> <name> --url <url> [--keyword <text> | --no-keyword <text>] [--every 60s] [--timeout 10s]`
- `davy monitor add <project> <name> --heartbeat [--every 1h] [--grace 5m]` prints the ping path `/api/heartbeat/<token>`.
- `davy monitor list` shows each monitor with its state, last check and open incident. `davy monitor remove <id>`.
- `davy monitor edit <id> [--project <slug>] [--name <name>] [--every <d>]`, and for an HTTP check `[--url <url>] [--keyword <text> | --no-keyword <text> | --status-only] [--timeout <d>]`, for a heartbeat `[--grace <d>] [--slug <slug> | --no-slug]`. It prints what `add` prints, or with `--json` the monitor as one row of `davy monitor list --json`. See [Editing a monitor](#editing-a-monitor).
- `davy channel add webhook <url> (--project <slug> | --all)` prints the channel ID and the signing secret, once. `davy channel add slack <url> (--project <slug> | --all)` and `davy channel add email <address> (--project <slug> | --all)` print the ID. An address is one `user@domain` of at most 254 characters, stored as a `mailto:` URL. The project is a flag because clap cannot read an optional positional before two required ones.
- `davy channel list` shows each channel with its unsent, failed, dropped and SDK dropped counts and where it goes: only the host of a webhook or Slack URL, because a Slack webhook URL is a credential, and the whole address of an e-mail channel. `davy channel remove <id>`. `davy channel test <id>` queues a `test` notice for that channel alone. Its `project` is `null` on a channel of every project.

Monitors and channels are added and changed from the CLI only. No MCP tool creates or edits them, so an agent cannot point the server at an address or silence an alert. The web UI shows monitors and has no form to change one.

### Editing a monitor

Removing a monitor deletes its checks and incidents, and a new heartbeat gets a new token, so `davy monitor edit` changes a monitor in place:

- It keeps the ID, the checks, the incidents, the state and the heartbeat token. Only the given flags change. At least one is required.
- The result is held to the limits of `add`: the URL, the name, the keyword, `every` with `timeout` or `grace`, and the slug, unique per project.
- A monitor keeps its kind. An HTTP flag on a heartbeat, or a heartbeat flag on an HTTP check, is refused. To change the kind, add a new monitor and remove the old one.
- A heartbeat whose `every` or `grace` changes expects its next ping by `last ping + every + grace`, as a ping sets it. A heartbeat that is `Pending`, or down after a missed ping, expects nothing until its next ping, as before.
- `--project` moves the monitor with its history. The new project's channels get its notices from then on. When it moves with an open incident, the channels of the new project alone get `monitor.down` for that incident, so the `monitor.up` that closes it does not come out of nowhere. Channels of every project got it already, and the old project's channels get no `monitor.up`.
- A monitor made by check-ins takes its schedule from the check-ins and is found by its project and slug, so only `--name` changes it. Every other flag is refused with that reason.
- The edit is one transaction. A check that started before it stores its result for the old settings.

### Not in this milestone

Status pages, regions and remote probes, ping, TCP, DNS and TLS expiry checks, custom request headers and bodies, escalation policies, on-call schedules, SMS and calls, alerts on escalating issues and on log queries, silences and maintenance windows.

### Done means

- Unit tests with literal values: the state machine for every pair of state and outcome; `Reach::Public` for a table of addresses, IPv4-mapped, NAT64 and 6to4 included; check outcomes against a local test server for status, keyword, keyword absence, timeout, a redirect to a refused address and a body over 1 MiB; the webhook signature of a fixed body, time and secret; the retry schedule; the outbox order per channel; the heartbeat route with a known, an unknown and a `/fail` token; the `sql` tool reading `NULL` for the three credential columns.
- `e2e/alerts.mjs`, run by `e2e/run.sh`, starts its own `davy serve` with a local site, a local webhook and Slack receiver and a local SMTP server, and one channel of each kind for all projects. The site answers 503 until the monitor is down, then 200. `@sentry/node` sends two new issues. A heartbeat gets a ping, a failure ping and a ping, then misses its window. Each channel must get the same 6 notices: one `issue.digest` of both new issues, `monitor.down` then `monitor.up` for the site, and `monitor.down`, `monitor.up`, `monitor.down` for the heartbeat. Every webhook signature must verify with the channel's secret, every e-mail must have its own `Message-ID`, and `davy channel list` must show nothing unsent or failed. It takes about 75 s, most of it waiting for the missed heartbeat.
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

- `davy setup --data DIR --public-url URL [--listen ADDR] [--mail-from ADDR] [--service systemd|launchd|none]` prepares a machine. It creates the data directory readable only by its owner, writes a random admin token to `DIR/token` with mode 0600 when none exists, records its flags in `DIR/setup.json` for `serve`, `doctor` and `mcp-config`, writes a launchd job into `DIR` when asked and prints JSON: the paths it wrote and the next commands. Running it twice changes nothing and prints the same JSON.
- Setup runs as the user the service runs as and never needs root. It refuses a systemd service for root or for a user that does not exist, because the server parses untrusted input. It refuses a directory that holds files and none of Davy's or that belongs to another user, because it makes the directory private, and it refuses a link in place of its own files. The commands that need root (installing the unit, `systemctl`) are printed, not run, so the person sees each one.
- The systemd unit is never a file in the data directory. `davy systemd-unit --data DIR --user USER` prints it, run by the operator and piped to `sudo tee`. It holds the binary, `serve --data DIR` and the user, all checked, and nothing read from `DIR`: a service user that an attacker took over could otherwise rewrite the unit that root installs and run code as root. Paths with a character systemd reads as syntax are refused, so no word needs escaping.
- The systemd unit has `Type=notify`. The server sends `READY=1` to `NOTIFY_SOCKET` once it listens, so `systemctl restart davy` returns when requests are answered and the `davy doctor` that follows finds the server up. Found by `e2e/install.sh`, where doctor ran before the server listened.
- The systemd unit runs the server with write access to the data directory only (`ProtectSystem=strict`, `ReadWritePaths`), no capabilities, no new privileges and a private `/tmp`. `davy serve` reads the admin token from `DIR/token` and the SMTP URL, which holds a password, from `DIR/smtp-url` when `DAVY_TOKEN(_FILE)` and `DAVY_SMTP_URL(_FILE)` are unset, and the flags it was not given from `DIR/setup.json`.
- `davy serve` also reads the admin token from the file named by `DAVY_TOKEN_FILE`, so a service file holds a path, not the token.
- `davy doctor --data DIR [--json]` checks a running install. Each check has a status (`ok`, `warn`, `fail`) and, unless it is `ok`, the command or setting that fixes it. It checks that the data directory belongs to the user doctor runs as and that others cannot read it, the token's presence and length, that the server answers at the public URL, that its `/mcp` refuses a request without the token and answers an MCP `initialize` with the token in `DIR/token` by a JSON-RPC reply of at most 64 KiB, that the public URL is `https` unless it is a loopback address, that the SMTP server accepts a connection when one is set, and free disk space. It sends nothing to the internet beyond the public URL and the SMTP server, and it sends the token only over `https` or to a loopback address, without a proxy. The exit code is 1 when a check fails.
- `davy mcp-config --client claude-code|codex|cursor (--data DIR | --url URL) [--show-token]` prints what to add to which file of that client: `.mcp.json` for Claude Code, `.cursor/mcp.json` for Cursor, `~/.codex/config.toml` for Codex. The config reads the token from `DAVY_TOKEN` in the syntax of each client, so it can be committed. `--url` serves a laptop that has no data directory. A plain `http` URL is refused unless its host is a loopback address such as `127.0.0.1` or `[::1]`, because the agent would send the token unencrypted. Doctor and mcp-config treat the name `localhost` as any other name, since a resolver can send it to DNS. The token itself is printed only with `--show-token`, which needs `--data`, and then the output says to keep the file out of version control.
- Every CLI error says what to run or set next.

### Feedback from agents

- MCP gets a sixth tool, `submit_feedback`. An agent calls it when a tool was missing, returned the wrong thing or needed a workaround, with what it tried, what happened and a proposed fix. Its description asks for one call per problem and no data from the errors or logs it read.
- Feedback stays on the server: one row in a `feedback` table with the time, the client name and the text. `/mcp` keeps no sessions, so the name comes from `io.modelcontextprotocol/clientInfo` in the request's `_meta` and otherwise from `User-Agent`. That is a hint, not an identity: the TypeScript SDK 1.x sends neither name, so `e2e/run.sh` sees `node`. Each field holds at most 4000 characters, and only the newest 1000 rows stay, so an agent in a loop cannot fill the disk. `davy feedback --data DIR [--json]` lists it. Davy sends it nowhere by default. A channel created with `--feedback` also receives each new item, so a team can route it to Slack and act on it there.
- The agent eval reads `davy feedback` after each run. Each item becomes a fix in the docs, the CLI or a tool, or a line in this section on why not.

### Agent eval

`e2e/eval/run.sh codex|claude RUNS` starts a Debian container with systemd, Node 22, `sudo` and the coding agent, copies the small app in `e2e/eval/app` to `/app` and runs the agent once with the prompt in `e2e/eval/prompt.txt` and no other help. A run passes when three things hold afterwards: `davy doctor` reports no failed check, a fresh MCP client using the token in the data directory finds the test error with `sql`, and the app, started with `npm start` in an environment that holds no `SENTRY_DSN`, reports the `TypeError` that `GET /orders/42` throws. The results, the agent's events, the app's diff and `davy feedback` of each run stay in `.scratch/eval/results/`.

Until 0.1.0 is released, the prompt differs from the README's in two ways. The docs are a copy of the repository at `/davy`, because the repository is private, and step 1 uses a binary built from the same commit with its `SHA256SUMS` instead of a download. The answers to the questions the docs tell the agent to ask are in the prompt, because no person watches the run: the public URL `http://127.0.0.1:8000`, no TLS proxy, no SMTP server.

Results of 2026-11-20, with the default model of each agent and a subscription login:

| Agent | Model | Passed | Median | Range |
|---|---|---|---|---|
| Claude Code 2.1.322 | `claude-sonnet-5-5` | 5 of 5 | 34 s | 31 to 34 s |
| Codex 0.170.0 | its default | 5 of 5 | 65 s | 47 to 114 s |

A Claude Code run read about 13 thousand new and 200 thousand cached input tokens and wrote about 2.4 thousand, 0.10 USD at API prices. A Codex run read 210 to 390 thousand input tokens, over 90 % of them cached, and wrote under 2 thousand. Each agent followed `install.md` in order, checked the binary against `SHA256SUMS`, installed the unit with `davy systemd-unit` and connected itself, and `davy feedback` was empty after every run.

The first round, judged only by doctor and the test error, passed 5 of 5 for both agents, but in 2 of the 5 Claude Code runs the DSN lived only in the shell that sent the test error, so the app would have sent nothing after a restart. `docs/connect/node.md` said to put the DSN next to the app's other variables, and this app had none. It now says that such an app gets a `.env` loaded by `node --env-file-if-exists=.env` in its `start` script, and the eval starts the app to check it. In the second round all 10 runs did that.

The commands that failed during the runs came from the container or the agent, not from the docs, and each agent recovered on its own: `rg` and `gh` are not installed, `systemctl` without `sudo` finds no D-Bus in the container, and a hand-written MCP request without `Accept: application/json, text/event-stream` got the 406 the MCP specification asks for.

`davy mcp-config` prints the target file and the config together, so Codex cut the TOML out with `sed` or ran `codex mcp add` with the same values. It stays that way: for Claude Code and Cursor the config is merged into a JSON file that may already exist, which an append cannot do, and the output names the file in each case.

### Safety

The agent runs these commands with the person's shell rights. The docs never tell it to put the token on a command line, to disable TLS checks, to widen file permissions or to pipe a download into a shell. The install step downloads the binary, checks it against `SHA256SUMS` and, when `gh` is present, against its build provenance, and only then moves it into place.

### Done means

- Unit tests with literal values: `setup` on an empty and on a prepared directory, the systemd unit `systemd-unit` prints and the launchd job `setup` writes, a `doctor` table where each broken install (open permissions, short token, server down, plain `http` public URL, refused SMTP) reports its own check and fix, and `mcp-config` for each client.
- `e2e/install.sh` starts a clean Debian container with only the binary, extracts the `sh` blocks from `docs/install.md`, `docs/connect/node.md` and `docs/connect/agents.md` and runs them in order as a user with `sudo`, leaving out a block marked `<!-- e2e: skip, reason -->`. `davy doctor --json` must report every check `ok`, `@sentry/node` sends an error, and the MCP `issue_context` tool returns it.
- An agent eval: Claude Code and Codex, each in a fresh container with a small Node app and nothing but the prompt, finish with the app's test error visible over MCP. Five runs each. The pass rate and the median time go into this section, and a failed run becomes a fix in the docs or the CLI.
- `scripts/check` passes.

## Milestone 8: release 0.1.0

The first version someone can download, verify and run without building it. `RELEASING.md` and the security program in `SECURITY.md` are the spec. This milestone builds what they mark as planned.

### Done means

- `scripts/release X.Y.Z` cuts a release as `RELEASING.md` describes, and refuses to run on a dirty tree, off `main`, behind `origin/main` or with an empty `## Unreleased`.
- A test tag runs the release workflow end to end. Its artifacts verify with `gh attestation verify`, `sha256sum -c SHA256SUMS` and `cosign verify`, and `cargo audit bin` reads the dependency list from a downloaded binary.
- The release workflow passes `zizmor`, pins every action to a SHA, and only its publish job can write.
- A fuzz target exists for every parser in `SECURITY.md`, each ran for at least an hour without a crash, and CI runs each for a few minutes every night.
- One cleanup pass over the whole codebase before the tag: leaky abstractions, copy-paste, wrappers with one caller and workarounds whose cause is gone. Each finding is fixed or recorded as a reason to keep it.
- The internal penetration test report exists, and every critical and high finding is fixed with a test.
- The owner's commits and the release tag are signed and show as verified on GitHub.
- The e2e suite passes against the downloaded release binary and the container image.

### Results

Fuzzing, measured on 2026-11-20 with `FUZZ_CPUS=5 fuzz/docker 3600` on an M-series laptop: all nine targets ran for 3,601 seconds at once, without a crash, a timeout or a memory limit hit.

| Target | Runs | Coverage | Peak RSS |
|---|---|---|---|
| `artifact_bundle` | 11,757,240 | 2,219 | 558 MB |
| `envelope` | 8,242,563 | 7,846 | 592 MB |
| `event` | 11,363,176 | 7,286 | 594 MB |
| `logtail_json` | 13,764,929 | 3,080 | 611 MB |
| `logtail_msgpack` | 7,392,232 | 2,057 | 1,651 MB |
| `otlp_json` | 14,558,134 | 4,147 | 599 MB |
| `otlp_protobuf` | 13,705,341 | 1,800 | 644 MB |
| `sentry_log` | 13,956,866 | 2,499 | 657 MB |
| `sourcemap` | 14,367,734 | 4,191 | 600 MB |

The peak of `logtail_msgpack` is libFuzzer's whole process, not one input: a further 90 seconds with `-malloc_limit_mb=128` found no single allocation over 128 MB.

After the pentest fixes each log parser target also asserts that a body it accepts has a price in `src/heap.rs`. That found three bodies that parsed without one: a protobuf group, invalid UTF-8 inside an unknown JSON field and the MessagePack marker `0xc1`. The server refused all three, with the wrong status. After the fix in `572f846`, `fuzz/docker 60` ran all nine targets without a failure.

The internal penetration test of `e2c7701` is in `docs/security/pentest-0.1.0.md`. It found one high, two medium, seven low and six informational findings. The high and both mediums are fixed with tests, and so are most of the rest. What stays is accepted there with a reason.

The release workflow ran end to end on the test tag `v0.1.0-test.3` (run 44816788766), unsigned, because the repository is private: the four binaries, the image for both architectures, the checksums and the dependency list verified, and the e2e suite passed against a downloaded binary and the image.

The cleanup pass surveyed the code at `7b4fabf` and found 28 items: duplicated code, dead fields, loose visibility and stale docs. 24 are fixed, one in part, in the `refactor`, `fix` and `docs` commits up to `ca05297`. These stay, each for a reason:

- `ErrorEvent::parse` reads the clock itself, because the fuzz targets call it with one argument.
- `create_project` reads the clock itself. `projects.created_at` is never read, so a fixed time would help no test and touch about 20 call sites.
- `IssueDetail.last_seen_ms` stays, because the issue context falls back to it when no event of the issue is stored.
- `http::serve` keeps its six arguments. Taking `serve::Resolved` would make the library depend on a CLI type.
- A `memory()` test helper of a few lines stays in four test files. Small copies like it read more easily in place than through a shared helper.

## Milestone 9: web UI for one team

The CLI and MCP serve agents and whoever lives in a terminal. A team also needs a place to see what broke, read a stack trace, search logs and check a monitor without a shell on the server. This milestone gives one team that place, inside the same binary. Roles, SSO and an audit log stay in the closed cloud (see "License and open core").

A person in the UI reads everything, changes the status of issues and manages their own account. Everything that makes Davy reach a URL or decides who gets in stays on the host: monitors, channels, projects and users are created with the CLI. Self-hosted Davy reaches any address (`net::Reach::Any`), because only someone with a shell on the host could type a URL. A form that let every account type one would let every account make Davy fetch internal addresses.

### Shape

- **A single-page app embedded in the binary.** React and TypeScript, built with Vite into `ui/dist/` and embedded at compile time. The browser loads it from `/ui/` and talks to a JSON API under `/ui/api/`. `GET /` redirects to `/ui/`, and `POST /` stays the Logtail route. No Node at run time.
- **Why not server-rendered HTML.** Server-rendered pages need no JavaScript toolchain, and that is their real advantage. The views this milestone needs are interactive: a stack trace that folds frames, filters that change a list without a reload, a log histogram you drag to zoom, keyboard navigation and dialogs that must be accessible. Built by hand, each is a weaker copy of a tested component. With React the UI uses an accessible primitive (Radix) where one is hard to build right, today only the popover of "Resolve in release". The rest is plain elements with roles and keys.
- **Every view state lives in the URL.** Filters, the time range, the selected event and the log query are search parameters. A link from a person, an alert or an agent opens the same view. The CLI prints these links with `--public-url`.
- **The API types come from Rust.** Every API response is a Rust struct with `#[cfg_attr(test, derive(ts_rs::TS))]`, so `ts-rs` is a dev dependency only. `scripts/check` fails when the generated `ui/src/api/types.ts` differs from the committed one, and a round-trip test parses real responses with the generated types.
- **Numbers JavaScript can hold.** A JSON number is exact in JavaScript only up to 2^53. Log times are unix nanoseconds, near 1.7 × 10^18, so the API sends them as decimal strings. Every cursor is an opaque string the server made and the UI passes back unchanged.
- **A binary without the UI still builds.** `ui/dist/` holds only a `.gitkeep` in git. `cargo build` without a UI build serves a page under `/ui/` that says the binary was built without it and names `scripts/ui`. CI, the e2e suite and the release workflow build the UI first.
- **Accounts are a trait.** The routes take an `Accounts` implementation that turns a request into a `Person` or refuses it. The open crate implements it on the SQLite tables below. The closed cloud crate brings its own, with tenants, roles and SSO, and reuses the routes and the views. UI reads go through the same library functions as the CLI and MCP, so the cloud's tenant scope reaches them too.

### People and sessions

Davy has had one secret, the admin token, for agents and `sentry-cli`. People get their own accounts, because a shared password cannot be taken back from one person.

```sql
users(id INTEGER PRIMARY KEY, email TEXT UNIQUE NOT NULL, password TEXT, created_at INTEGER NOT NULL)
sessions(hash BLOB PRIMARY KEY, user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE, created_at INTEGER NOT NULL, used_at INTEGER NOT NULL)
invites(hash BLOB PRIMARY KEY, user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE, expires_at INTEGER NOT NULL)
```

- An e-mail is stored trimmed and lowercased, so `Ana@x.cz` and `ana@x.cz` are one user.
- `davy user add <email>` creates a user without a password and prints a one-time link, `<public-url>/ui/welcome#<secret>`. The link works for 24 hours and once. The secret sits after `#`, so it never reaches a proxy log, and the page removes it from the address bar (`history.replaceState`) before anything else runs. The page posts it in the body with the new password, and the answer logs the person in. This works without SMTP, and an agent that installs Davy can hand the link to its person.
- `davy user reset <email>` deletes the user's open invites and sessions and prints a new link. `davy user remove <email>` deletes the user, their sessions and invites. `davy user list` prints the users. All four take `--json`. They write from their own process, like `project create`.
- In the UI a person changes their own password, which ends their other sessions, and can log out everywhere.
- Passwords are hashed with Argon2id at the OWASP settings (19 MiB, 2 passes, 1 lane) and stored as PHC strings. A password has at least 12 characters and at most 1,024 bytes, and is not one of the 3,000 most common passwords of that length in the SecLists list of a million, compared without case (ASVS 6.2.4). A login for an unknown e-mail hashes a fixed dummy, so its timing does not say which e-mails exist.

### Login under attack

- A body sent to login, welcome or a password change is read only up to 4 KiB, and only for 5 seconds. A browser sends it with the headers.
- At most two hashes run at once, so hashing never holds more than 38 MiB. Up to 32 more logins wait their turn for at most 3 seconds. A login that finds the queue full, or no slot after 3 seconds, gets `429` with `Retry-After: 1`. The UI pentest found that refusing the third login at once let 30 parallel logins refuse 26 of them. A login or password change still in its backoff wait is refused before it takes a place in the queue, so a guesser who keeps trying cannot hold places that others need. Only the attempt that gets a slot counts, so a busy queue is no failed attempt.
- These bodies pay from no memory budget. A budget bounds nothing for a body smaller than its connection already holds, and slow bodies that spent it refused everyone. In the UI pentest, 800 slow bodies held open made the 2 MiB budget this replaces refuse every login. The 5 second limit ends a slow body instead, and the test `slow_login_bodies_do_not_refuse_a_login` holds 1,000 of them open while a login works.
- After failed logins for one e-mail, the next try is refused with `429` and `Retry-After` for 2^n seconds, up to 15 minutes. Unknown e-mails count the same way, so the answer does not say whether an e-mail exists. The counts live in memory in two tables. Users count by their id in a table that nothing pushes out, which the users of one server keep small. E-mails without a user count by 16 bytes of their BLAKE3 hash in a table of at most 100,000, under about 10 MB, and the oldest goes when it is full. A flood of made-up e-mails therefore pushes out only other made-up e-mails. Until the UI pentest both shared one table that dropped the entry with the fewest attempts, and a second read of the fixes found that this let a guesser push out a user's count after every guess, about 30 guesses a second once the table was full. What a flood still buys: a guesser who raised the wait of an e-mail and then sends 100,000 attempts within it, about 12 minutes at the 140 hashes a second of two slots, learns that the e-mail has no user. A password change in an open session has a count of its own.
- This slows a guesser to a few tries an hour, but it also lets anyone who knows a colleague's e-mail keep them from logging in for 15 minutes at a time. A session already open is not touched, and limits per client address are the reverse proxy's job. Both are written into `SECURITY.md`.

### Sessions and cookies

- A session is 32 random bytes in the cookie `__Host-davy_session` (`Secure`, `HttpOnly`, `SameSite=Lax`, `Path=/`). The database keeps only its blake3 hash. It ends 14 days after its last use and 90 days after login. The lookup joins `users`, so a removed user's session ends even before the cascade runs.
- `Lax`, not `Strict`, because a link to an issue opened from Slack or an e-mail is a navigation from another site. With `Strict` the browser would leave the cookie out and every alert link would land on the login page. A write is a JSON `POST` with an exact `Origin` (below), and `Lax` does not send the cookie on a `POST` from another site.
- The login page takes `next`, the page to open after login. Davy follows it only when it is a path under `/ui/`, so it cannot redirect elsewhere.
- Chrome and Firefox keep a `Secure` cookie from `http://localhost`, so a local Davy works before TLS is set up. Safari does not, and the login page says that it needs HTTPS when the cookie did not stick. Any other host needs the HTTPS that `SECURITY.md` already requires.

### Where the API checks access

- The whole `/ui/api/` router sits behind a `route_layer` that asks `Accounts` for a `Person`. Login, logout and welcome live on a separate router without it. A test lists every route of the app and fails if a `/ui/api/` route outside those three answers without a session.
- A write must have `Content-Type: application/json` and an `Origin` that is exactly the public URL: scheme, host and port. Without a public URL, it is exactly the address Davy listens on. A write without `Origin` is refused. The loopback rule of `/mcp` (`http://localhost:*`) is not enough here, because a cookie is sent to every port of a host. Any dev server on the same machine could then act as the person.
- The `Host` check of `/mcp` applies to `/ui/` too, against DNS rebinding. The `Access-Control-Allow-Origin: *` that ingest answers with is not sent under `/ui/`.
- Logout writes only when its session exists, so a flood of made-up cookies does not fill the writer's UI queue.
- The session is checked before the handler reads the body, and the writer does not check it again. A body of a write is therefore read within 5 seconds or refused, so a session that ends meanwhile, by logout everywhere or a password change, can finish at most a write already on its way.
- The text of an internal error goes to the server log. A browser gets `500` with a fixed message.
- `davy user add` refuses a public URL with plain `http` to a host other than this machine, because the browser would not keep the `Secure` cookie and the new password would cross the network in the clear.
- The session cookie is not accepted on `/mcp` or the upload routes, and the admin token is not accepted under `/ui/api/`. Each caller keeps one credential, as in the table in `docs/architecture.md`.
- Every `/ui/` response carries `Content-Security-Policy: default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; font-src 'self'; connect-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'`, plus `X-Content-Type-Options: nosniff`, `Referrer-Policy: same-origin`, `Cross-Origin-Opener-Policy: same-origin` and a `Permissions-Policy` that turns off camera, microphone and geolocation. `/ui/api/` and `/ui/welcome` answer with `Cache-Control: no-store`.
- No script and no `<style>` element runs inline. React, Radix and Motion set styles through the CSSOM, which this policy allows. A primitive that inserts a `<style>` element, as Radix's scroll lock for modal dialogs does, is not used. The e2e suite fails on any CSP violation the browser reports.

### Untrusted text in the browser

Anyone with a project's public key can send an event, so every title, message, frame, tag, breadcrumb and log line is attacker-controlled text.

- The UI renders it as text only. `scripts/check` fails on `dangerouslySetInnerHTML`, `innerHTML`, `outerHTML`, `insertAdjacentHTML`, `document.write`, `eval(` and `new Function` anywhere in `ui/src`. A grep, not an ESLint plugin, because it is the one rule that matters here and a grep has no dependencies.
- A URL from an event (`request.url`, a link in a breadcrumb) is a link only when it parses as `http:` or `https:`, and it opens with `rel="noopener noreferrer"`. Anything else is shown as text.
- Control characters and bidirectional overrides in a title or message are shown as visible escapes, so a title cannot hide or reorder what it says.
- The browser never loads anything an event names. There are no images, favicons or previews from event data. `img-src` allows only Davy's own origin.
- **Hand to an agent** (see Issue) copies a prompt with the issue's number and the names of the MCP tools, never its title, message or any other event text. Event text in a prompt would let anyone with the public key write instructions into a coding agent that has a shell.

### Reads cost memory too

- The issue view decompresses an event's payload and parses it. It pays for both from the memory budget, priced like an ingest body. It pays for the compressed blob, the decompressed JSON, then twice its parsed size, and gets `429` with `Retry-After: 1` when the budget is held.
- `EventView` keeps at most 256 frames per exception, as symbolication does, 200 breadcrumbs, 200 tags and 64 KiB of any one string. Each cut shows how many more there were. It also keeps 32 exceptions of a chain, 32 contexts, 200 fields of the user, of a context or of a breadcrumb, and 32 source lines either side of a frame's line, so no list of an event is unbounded.
- At most 8 log searches and histograms run at once, shared by every person. Another one gets `429` with `Retry-After: 1`, and the UI tries again.

### Writes go through the writer

Inside the server only `store::Writer` writes SQLite. The writer queue gains these jobs, each answered when it commits:

- `SetStatus { issues, status }` for resolve, resolve in a release, ignore and reopen, of one issue or up to 500 at once in one transaction, so a bulk change never half applies. `issues` is sorted, without duplicates and holds 1 to 500 ids by construction. The time of a resolve is part of `status`, `Resolved { in_release, at }`, since only a resolve has one. One missing issue refuses the whole change with `no issue {id}`. It replaced `Resolve`, and the MCP tool `mark_fixed` and `davy resolve` use it too.
- `Login { user, hash, at }`, `Logout { hash }`, `LogoutAll { user, except }`, `Welcome { invite, password, session, at }` and `SetPassword { user, password, keep }`.
- `Touch { hash, at }`, which writes a session's `used_at` at most once an hour.
- `SetRetention(change, by)` for how long a project keeps its logs and errors, from `POST /ui/api/retention` (`By::Person`) and the MCP tool `set_retention` (`By::Agent`). An unknown project refuses the change with `unknown project "slug"`, and an agent's change that keeps either kind shorter than now with the reason. The writer decides, so two changes at once cannot slip a shorter one past the check.

During an error storm the ingest queue is full, and that is when the team needs the UI most. The writer takes UI jobs from a second queue of 64, so a full ingest queue does not refuse a login or a resolve. UI jobs go first, but take at most half of a batch of 64 while ingest waits, so people who keep writing cannot stop ingest either.

### Views

The API pages with a cursor, never an offset, so a page costs the same at row 50 as at row 500,000. A list cursor is the sort's letter, its key and the issue id, such as `e12.345`, and the list refuses a cursor made for another order. Times in the API are RFC 3339 strings in UTC with milliseconds. Only a log line also carries its nanoseconds, as a decimal string.

The routes, all behind the session:

| Route | Answer |
|---|---|
| `GET /ui/api/me` | `Me`, the person logged in |
| `GET /ui/api/projects` | `Project[]`, by slug, each with its newest 50 releases |
| `GET /ui/api/issues?sort&project&status&environment&release&q&cursor` | `IssuePage`, 50 issues with the hourly counts of the last 24 hours |
| `POST /ui/api/issues/status` | `StatusChange` in, `204` out |
| `GET /ui/api/issues/{id}` | `IssueView`, the issue, its hourly counts and its releases |
| `GET /ui/api/issues/{id}/events?cursor` | `EventPage`, 50 events newest first, each saying whether its payload was kept |
| `GET /ui/api/issues/{id}/event?event` | `EventView` of that event, or of the newest kept one |
| `GET /ui/api/issues/{id}/logs?event` | `Logs` around that event, as `issue_context` finds them |
| `GET /ui/api/logs?q&from&to&project&level&before&after` | `LogPage`, 100 logs newest first and the cursor of the page before |
| `GET /ui/api/logs/histogram?q&from&to&project&level` | `LogHistogram`, the matches counted in at most 200 round buckets |

A route takes only the query keys it names, each at most once, and answers `400` naming the keys it takes. The project filter takes a slug. Environment and release filter through `issue_tags` (see "Load check"). There is no list of environments, since it would need a scan of every event, so environment is typed. Writes answer `404` when an issue is missing, without a read before the write, because the writer already refuses the whole change.

**Issues** (`/ui/issues`). A list of issues sorted by last seen, event count or first seen. Filters are project, status, environment, release and a text match on title and culprit. Each row shows title, culprit, level, status, event count, first and last seen, and a bar chart of the last 24 hours from `issue_hourly`. Select several issues to resolve, ignore or reopen them at once. Two covering indexes serve every filter and sort (see "Load check").

**Issue** (`/ui/issues/{id}`). The issue with its status actions: resolve, resolve in a release, ignore, reopen. The selected event (the latest by default) shows the exception chain with its frames (in-app frames open, the rest folded, the symbolicated position and context lines), breadcrumbs, tags, the request, the user, runtime, OS and browser. Then the events of the issue to step through with `[` and `]`, the releases it was seen in, and the logs around the event that `issue_context` finds. The compaction `issue_context` does for agents drops frames and fields to save tokens. The UI reads the payload through its own pure function from payload JSON to an `EventView`, with the limits above, tested on the captured fixtures. **Hand to an agent** copies a prompt that names the issue and the MCP tools `issue_context` and `mark_fixed`. This is the agent-first loop, started from the place a person found the error. The stepper moves only through events whose payload Davy kept (the first 50 of an issue, then every 100th), and says so under it, because the others have no payload to show.

**Logs** (`/ui/logs`). The query syntax of `davy logs`, a time range, project and level filters. Above the results is a histogram of matches over the range, from a new `logs::store::histogram`, at most 200 buckets. Drag across it, or pick one bar, to zoom the range to those buckets, and `Esc` zooms out. Results are newest first and page backwards. A row opens to show every attribute, and each attribute has a button that adds the query for its value, such as `attributes.order\.id:"ord-1001"`, to the search. **Live** asks for logs newer than the last one every 2 seconds while the tab is visible, and the histogram redraws every 10 seconds. Polling, not a held connection, so a proxy needs no streaming setup and a hidden tab costs nothing. A range that ends now is read as of the moment it was picked, so the list and the histogram count the same logs until Refresh.

- **Times.** `from` and `to` are nanoseconds since 1970, both ends included. Without `from` the range is the hour up to `to`, and without `to` it ends now.
- **Paging.** A page is the newest 100 matches. Many logs can share one nanosecond, so the cursor of the page before is the oldest time on the page and how many logs at that time the page showed, such as `1763600000123456789.3`. The next page reads up to that time and skips them. A cursor steps over at most 10,000 logs at one nanosecond. Past that the next page starts a nanosecond earlier, so paging never stops, and the rest of the logs at that nanosecond are not shown. A cursor older than `to` is ignored, so it skips nothing in another window.
- **Live** sends `after`, the time of the newest log it has less 2 seconds, because a log can be committed after its own time. The answer is the newest 100 logs after it, and the UI drops the ones it already shows. When the answer is full, more arrived than a page holds, and the page replaces the list instead of joining it. After 500 rows live starts the list over from the newest page.
- **The histogram** picks the narrowest round step from a fixed list, such as 500 ms, 15 s, 5 min or 6 h, from 1 ms up to 14 days, that splits the range into fewer than 200 buckets, and starts the first bucket at a multiple of the step, so the bars sit on round times. It counts with its own collector over the `ts` fast field of each daily index the range touches. tantivy's histogram aggregation works in `f64`, which holds a time of today only to the nearest 256 ns, so a bucket edge could take a log from its neighbour.
- **Load.** A log search or histogram runs on the blocking pool and holds one of 8 search places. A ninth at once answers `429` with `Retry-After: 1`, which the UI waits out, so a few open tabs cannot take every core from ingest.

**Uptime** (`/ui/monitors`). Monitors with their state, last check or ping and the response times of the last 24 hours from `checks`, each hour as a cell with a table form. A monitor's page (`/ui/monitors/{id}`) adds 7 and 30 day uptime, never rounded up so 99.999% does not read as 100%, its newest incidents with reasons, the newest checks of an HTTP monitor, and for a heartbeat the last ping and when it is late. Monitors are added with `davy monitor add`, so the empty list shows that command.

**Alerts** (`/ui/alerts`). Channels with their kind, project and host, how many notifications wait, failed or were dropped, apart from the dropped notices of monitors made by check-ins, and the notifications newest first with their delivery state, tries, when they were sent or retry next, and the last error. The rail filters by channel. A channel's URL path and secret are never shown.

**Projects** (`/ui/projects`). The server's public URL and its Logs, OpenTelemetry and MCP endpoints, then each project's DSN and key, each with a copy button, and the setup code from `docs/connect/` for the SDK picked in a tab list (Node, the browser, Python, Go, Logtail, OpenTelemetry). The project and the SDK are in the URL.

**Account** (`/ui/account`). Change the password, which logs out every other session, and log out every other session without a change.

**Setup** (`/ui/setup`). What the install by prompt has done so far, for the person who pasted the prompt and waits for the agent. The idea comes from Sentry's agent-first onboarding page, which lists the steps the agent reports. Davy lists only what it saw itself, so no step shows done unless it happened: the agent connected (the first `/mcp` request with the admin token), a project exists, the first error arrived, a frame was symbolicated with an uploaded source map, the first log arrived, a monitor exists and a channel delivered a notification. The first four are required, the rest optional. A step that is not done shows the command that does it or points to **Projects**, which has the code for each SDK, and an optional one says "Not set up", not a failure. A step that turns done is announced to a screen reader. The first `/mcp` request is the only fact not already in a table, so it gets one: `firsts(what TEXT PRIMARY KEY, at INTEGER NOT NULL)`, written once by the writer. The page asks again every 2 seconds while the tab is visible, like **Live**. `/ui/` opens on it until the first error arrives, so the link `davy user add` prints lands there. For this, `docs/install.md` moves `davy user add` to right after the server answers: the agent hands the person the link first, so they can watch the rest.

### What it looks like

The look was decided on a prototype of the issue page and the issues list, built on fixtures from the e2e SDKs. Of three directions the owner chose the one called Instrument. It should feel like calm, exact test equipment for someone who triages hundreds of issues. The prototype is in [`docs/ui-prototype/`](ui-prototype/index.html) and opens from disk with no server. Its issues 1 to 4, 9, 10 and 14 come from the e2e fixtures, the rest and every hourly count are made up. The app follows these rules:

- It is a tool people read for hours. Information density beats decoration, body text is a neutral system face, and a monospace face is for code, stack traces and log lines only.
- Light and dark follow the system setting, each with its own palette. There is no theme toggle.
- Icons are a small set drawn for Davy, not a generic icon package.
- Every chart has a table form, identity is never color alone, and each chart palette passes a color-vision check in both themes.
- Motion is short and serves a change of state: a folded frame opening, a row leaving a list after resolve. CSS transitions do it, with no motion library. Nothing is hidden behind an entrance animation, and `prefers-reduced-motion` turns motion off.
- WCAG 2.2 AA. Every action works from the keyboard: `/` searches, `j` and `k` move through a list, `x` selects, `r`, `i` and `o` resolve, ignore and reopen, `Enter` opens, `Esc` closes.

**Layout.** A list view has three regions. A left rail of about 190 px holds the projects, the status and environment filters, and the sort, which the browser remembers. Every sort is newest or largest first, and the rail shows no counts per project, because the list API answers pages and no totals. The middle is the list in rows of 30 px, with the columns that **Views** names. A right inspector of about 310 px shows the row under the cursor without leaving the list: the title, the culprit, the nearest in-app frame with its crash line and source-map origin, counts, first and last seen, the releases and the 24-hour strip. `Enter` opens the issue page. It keeps the rail and gives the event the rest of the width, with tags and contexts as a two-column grid of keys and values, never chips. A bar at the bottom names the keys of the view. A narrow window loses the inspector first, then folds the rail behind a button.

**The heat strip.** It is the one signature of the look. The last 24 hours of an issue are 24 small cells in one hue, light to dark, on one scale for every row. In a column of rows the eye finds a spike without reading a number. Hover or the arrow keys read out the hour and the count, and the same numbers sit in a visually hidden table. The issue page shows the strip large, with an hour axis and the count in each cell. Cells with few events stay close to the surface color on purpose, and their counts are in the readout and the table.

**Color.** One palette per theme, a neutral graphite with a slight olive cast, and one accent, verdigris, for focus, the cursor row and the selected state. Regions part by tone, the rail and the panels a step off the list surface, not by a border on every box. Shadows only lift tooltips and popovers. The values from the prototype, contrast measured against the list surface:

| Token | Light | Dark | Contrast (light, dark) |
|---|---|---|---|
| list surface | `#fcfcfb` | `#1a1b17` | |
| rail | `#efefe9` | `#151613` | |
| panel | `#f4f4ef` | `#22231e` | |
| ink | `#1d1e1a` | `#e8e7de` | 16.3, 14.0 |
| muted ink | `#54554d` | `#b0afa2` | 7.4, 7.8 |
| faint ink | `#62635a` | `#a3a295` | 5.9, 6.7 |
| accent | `#17695a` | `#6cc6b1` | 6.4, 8.5 |
| cursor row | `#d7ebe5` | `#233a33` | |
| alarm (fatal, error) | `#a3302a` | `#e5958a` | 6.8, 7.4 |
| warning | `#715b00` | `#cdbf7c` | 6.4, 9.4 |
| heat, empty to peak | `#e4e6dd` to `#0b463b` | `#2a2b25` to `#bdf0de` | |

Every text color keeps at least 4.7:1 on the cursor row too. Fatal and error share the alarm color, and info uses the muted ink, so the status set has two hues. Each level has its own drawn mark and its word next to it, because the two hues alone fail for deuteranopia in the light theme (an OKLab distance of 1.9, measured).

**Type.** `system-ui` at 13 px for text, `ui-monospace` for code, frames, ids and times, tabular figures for every count. Weight and size make the hierarchy. No web font is loaded.

**Marks.** The Davy mark is a D built from five rounded bars, its middle bar in the accent, like rows of the strip. Icons for levels, statuses, folding, copying and the source-map origin are drawn for Davy.

### Front-end dependencies

npm is a supply chain of its own, and Davy is a security product. The rules:

- pnpm with a frozen lockfile. Dependency install scripts do not run (pnpm's default), and a version newer than 7 days cannot be added (`minimumReleaseAge: 10080`, in minutes). That setting checks a version when it enters the lockfile, not again later.
- A short list of runtime dependencies, each named here with the reason: `react` and `react-dom`; `@tanstack/react-router` for typed search parameters; `@tanstack/react-query` for cached API reads; `@radix-ui/react-popover` for the one popover, with focus and dismissal done right. No chart library: the log histogram is at most 200 SVG bars drawn in about 200 lines, where `uplot` would add about 50 KB. Uptime decides again when it arrives. No CSS framework and no motion library: one plain stylesheet holds the tokens of "What it looks like", and CSS transitions are enough for the motion it allows. A new runtime dependency needs a line in this list.
- `scripts/check`, and so CI, fails on any known vulnerability in `ui/pnpm-lock.yaml` (osv-scanner), and on a license outside MIT, ISC, BSD, 0BSD, Apache-2.0 and Unlicense in a package that ships in the bundle (`scripts/ui-licenses.mjs`). Unlicense is there for `isbot`, which the router brings. The release SBOM lists the npm packages next to the crates.
- At most 200 KB of JavaScript compressed for the first load, checked by `scripts/ui-pack.mjs` on every build. It writes a Brotli copy next to each text file, and Davy sends that copy with `Content-Encoding: br` and `Vary: Accept-Encoding` when the browser accepts `br`. Assets carry content hashes and are served with `Cache-Control: immutable`. `index.html` is never cached. The binary grows by at most 2 MB.

### Order of work

1. Accounts and sessions: the tables, `davy user`, the writer jobs and their second queue, login, logout, welcome, `Accounts` and the route layer, the security headers and the `/ui/` serving with the no-UI page. Contract tests first.
2. The design prototype of the issue page and the list, then the direction written here.
3. The app shell, then Issues and Issue.
4. Logs with the histogram and live.
5. Uptime, Alerts, Projects, Setup and Account.
6. An internal pentest of the UI routes, recorded in `docs/security/`, before the release that ships the UI.

### Not in this milestone

Creating monitors, channels, projects and users from the UI, which needs roles and a public-only `Reach` for URLs typed in a browser. Dashboards and saved queries, alert rules beyond channels, traces, more than one team, roles, SSO, an audit log, translations, and a mobile layout beyond a readable single column.

### Done means

- Contract tests with literal values: every `/ui/api/` route answers `401` without a session, with an expired one and with one of a removed user; a write answers `403` with an `Origin` on another port of the same host, without `Origin` and without JSON; login answers the same way for a wrong password and an unknown e-mail, and `429` at once when both hash slots are busy; a welcome link works once and not after 24 hours; `next` outside `/ui/` is ignored; the admin token is refused under `/ui/api/` and the cookie on `/mcp`; every `/ui/` response carries the headers above and no `Access-Control-Allow-Origin`.
- A store test: a bulk `SetStatus` of 500 issues commits all or none, and a login commits while the ingest queue is full.
- Unit tests: `EventView` from each captured fixture gives the exact frames and fields, and cuts an event with 10,000 breadcrumbs to 200 with the count of the rest; the log histogram counts a known set of logs across two daily indexes into the right buckets; a link from an event is a link only for `http` and `https`; a log time of `1763600000123456789` survives the round trip through the API and the generated types.
- `e2e/ui.spec.ts` runs Playwright against the built binary: `davy user add`, open the link, set a password, see the issue a real Sentry SDK sent, read its in-app frame, resolve it, find a log with a query and zoom the histogram, open a monitor, change the password. axe-core reports no violation on any view, and the browser reports no CSP violation. The issues part runs today, after the Sentry SDKs have sent their errors: it checks axe in light and dark on the welcome, list, issue and login views, and resolves the sentry-node-7 `TypeError` with `r`, so the later steps of `e2e/run.sh` keep the other three issues. The log step runs since the Logs page: it finds a log by an attribute, adds a search from an attribute button, zooms by dragging across the histogram, and sees a log sent while live is on appear within 6 seconds. The monitor and password steps join when their views do. axe checks after colour transitions end, because a button half way through its fade fails contrast.
- A load check: the issues list and the log search each answer in under 100 ms at the 95th percentile with 100,000 issues and 10 million logs in the data directory, measured and recorded here. Both halves are in "Load check" below.
- `scripts/check` passes, including the type diff, the lint ban, osv-scanner and the bundle budget.

### Load check

`bench/load-issues.sh` fills a store with 100,000 issues in 10 projects, 3 events each, and times 200 requests to `GET /ui/api/issues` per case over one keep-alive connection. Measured on 2026-11-23 on the developer laptop with a release build, load average about 5 from other work:

| Case | p95 before | p95 after |
|---|---|---|
| unresolved, last seen (the default view) | 8 ms | 0.5 ms |
| unresolved, most events | 80 ms | 5.9 ms |
| unresolved, first seen | 87 ms | 6.4 ms |
| one project, unresolved, most events | not measured | 0.7 ms |
| text in title or culprit, many hits | 101 ms | 7.6 ms |
| text, one hit | 110 ms | 6.0 ms |
| text of two characters, no hit | not measured | 29.8 ms |
| environment `staging` | 338 ms | 3.4 ms |
| release `v1.7` | 487 ms | 1.1 ms |

Before is the schema of `1caa0be` on the same data. After, the worst case is 29.8 ms. Three changes, all in migration 7:

- **Two covering indexes instead of one per sort.** `issues(status, last_seen, event_count, first_seen)` and the same led by `project_id` hold every sort key, so the ids of a page come from the index alone and only those 50 rows are read from the table. Davy never runs `ANALYZE`, and without statistics SQLite picked the old `(project_id, status)` index for a status filter and sorted every row in a temporary tree. One index per filter and sort pair would be faster still on the sorts but costs nine index writes on each event that bumps an issue.
- **A trigram full-text index for text.** `issues_text` is an FTS5 table over `title` and `culprit`, kept in step by triggers. A search of three characters or more matches the phrase there, ignoring case, also outside ASCII. A shorter search cannot use trigrams and scans the rows, ignoring ASCII case only: 29.8 ms when nothing matches, sub-millisecond when 50 rows match early.
- **Tags rolled up per issue.** `issue_tags(key, value, issue_id)` holds each environment and release an issue's events named, written by a trigger on `events`. The filter was an `EXISTS` over the issue's events for each issue.

What it costs. A Python prototype on the same data measured an event that bumps an issue at 24 µs before and 31 µs after, warm. The first start after the upgrade builds the text index and reads every event once for the tags: about 3 s for 100,000 issues and 300,000 events. A page still walks an index past the rows it does not show, so that work grows with the store. At 1,000,000 issues the slowest cases should take about ten times as long (a guess, not measured).

The first measurement ran under `taskpolicy -b`, which put the server on the background CPU and I/O class. Every case came out about five times slower: 49 ms instead of 8 ms for the default view. Benchmarks run at normal priority, and the script says so.

`bench/load-log-search.sh` writes 10,000,000 pino-shaped logs spread over the last 24 hours in 4 projects straight into the log store (`examples/fill_logs.rs`, 86 s, 1.3 GB in two daily indexes), then times 50 requests per case over one keep-alive connection. Measured on 2026-11-23 on the same laptop, release build, load average about 5:

| Case | Rows | p95 |
|---|---|---|
| page, last hour | 100 | 9.3 ms |
| page, whole 24 hours | 100 | 33.9 ms |
| page, 21 pages back | 100 | 34.6 ms |
| page, one project | 100 | 14.7 ms |
| page, errors | 100 | 9.0 ms |
| page, text `timeout` | 100 | 8.2 ms |
| page, `res.statusCode:500 AND req.method:post` | 100 | 13.9 ms |
| page, text with no hit | 0 | 5.8 ms |
| live, nothing newer | 0 | 1.9 ms |
| histogram, last hour | 406,727 | 4.7 ms |
| histogram, whole 24 hours | 9,990,060 | 46.4 ms |
| histogram, errors | 299,795 | 19.1 ms |
| histogram, text `timeout` | 299,795 | 7.7 ms |
| histogram, one project | 2,495,530 | 20.5 ms |

The worst case is 46.4 ms, counting every log of the day. A page sorts by the `ts` fast field and reads only the 100 documents it returns. The histogram reads the `ts` column of every match, so it grows with the matches, not with the store: about 4.6 ns per log here.

## What else the SDKs send

The SDKs send more than errors and logs. Three of those items say something an agent cannot learn elsewhere, so Davy keeps them in SQLite, where `sql` reads them.

**Client reports.** An SDK counts what it threw away before sending, per reason and category, and sends the counts in a `client_report` item every minute or so and on flush: errors `before_send` returned `null` for, transactions the sample rate skipped, logs a full queue lost, envelopes refused with `429`. Without them, an app that reports no errors looks healthy even when its `before_send` drops everything. Davy adds the counts up per project and UTC day:

```sql
client_reports(day INTEGER, project_id INTEGER, reason TEXT, category TEXT, quantity INTEGER,
               PRIMARY KEY(day, project_id, reason, category)) WITHOUT ROWID
```

`day` is midnight UTC in unix milliseconds, so one row grows by addition and a busy app adds a few rows a day. A reason or category must be at most 64 characters of `a-z`, `0-9` and `_`, which every name the SDKs send is. Rows are kept 90 days and the sweep deletes older ones.

**User feedback.** The SDKs' feedback widget and `Sentry.captureFeedback` send a `feedback` item, an event whose `contexts.feedback` holds `message`, `name`, `contact_email`, `url` and `associated_event_id`. Older SDKs send a `user_report` item with `event_id`, `comments`, `name` and `email`. Both become one row:

```sql
user_feedback(id INTEGER PRIMARY KEY, project_id INTEGER, received_at INTEGER, event_id TEXT,
              issue_id INTEGER, message TEXT, name TEXT, email TEXT, url TEXT)
```

`issue_id` is looked up when the row is written, from the event it names in the same project, so `issue_context` reads an issue's feedback through an index instead of joining every event of the issue. The SDK sends the error first and the user answers later, so the event is almost always there. Feedback that arrives before its event, or names an event Davy did not store, stays unlinked and is still in `sql`. Text is trimmed, control characters become spaces and each field is cut to its limit in the table under [Sizes and counts](#sizes-and-counts).

**Safety.** The DSN of a browser app is public, so anyone can send both items. What they can do with it:

- Fill `user_feedback` with up to 1,000 rows per project and day, at most about 26 MB a day when every field is at its limit in four-byte characters. Past that, feedback is refused and counted in `client_reports` under the reason `davy_daily_cap`, so an operator sees the flood.
- Add up to 100 made-up reason and category pairs per project and day. Counts of pairs that already exist keep growing, so a real SDK's reports still land.
- Inflate counts. A report says what an SDK claims, not what happened, and the docs say so.

Both items count against the request memory budget while they are parsed, like events. The writer stores them after the envelope's event, so a full queue costs a report or feedback, not an error. The e-mail and name a user typed are personal data. The `sql` tool and `issue_context` show them to whoever holds the admin token, the same people who can already read event payloads with user data in them.

**Sessions.** Release health asks how many sessions of a release crashed. Browser and mobile SDKs send a `session` item when a visit or an app run starts, again on its first error or a crash, and when it ends. Server SDKs count requests instead and send a `sessions` item with counts per minute (`exited`, `errored`, `crashed`, `abnormal`) about once a minute and on flush. Both name a `release` and an `environment`, and a session names the user as `did`. Davy adds them up per project, hour, release and environment:

```sql
release_sessions(project_id INTEGER, hour INTEGER, release TEXT, environment TEXT,
                 started INTEGER, errored INTEGER, crashed INTEGER, abnormal INTEGER,
                 PRIMARY KEY(project_id, hour, release, environment)) WITHOUT ROWID
release_users(day INTEGER, project_id INTEGER, release TEXT, environment TEXT, user INTEGER, flags INTEGER,
              PRIMARY KEY(day, project_id, release, environment, user)) WITHOUT ROWID
session_seen(project_id INTEGER, sid BLOB, flags INTEGER, started INTEGER,
             PRIMARY KEY(project_id, sid)) WITHOUT ROWID
```

- **Each session counts once.** One session arrives as several updates, each with the whole state so far. `session_seen` keeps the flags already counted for each session id (started, errored, crashed, abnormal), and an update adds only the new ones. A session first seen in a later update still counts as started, so a lost first update costs nothing. The row keeps the start the first update named, and later updates cannot change it, so they can neither move the session to another hour nor keep its row alive. The row goes when that start is more than 5 days old, the same moment the session stops being accepted, so a replay after that is refused instead of counted again.
- **The hour is when the session started**, not when the update came, so a crash an hour into a session lands in the session's hour. A session that started more than 5 days ago or over an hour ahead is refused and counted in `client_reports` as `davy_out_of_window`.
- **`errored` includes `crashed`**, as the SDKs count them. Crash-free sessions are `100 * (started - crashed) / started`, floored to two decimals.
- **Users** come only from `did`. Davy stores the first 8 bytes of BLAKE3 over the project id and the id, so the table holds no e-mail or user name and the same person in two projects is two values. A user counts once per release however many days they come back. `flags` remembers whether any of their sessions errored or crashed that day.
- **Retention** is 90 days for `release_sessions` and `release_users`. The sweep deletes older rows.

`release_health` sums these per release and environment since `since` and joins the users of the same days. A release with sessions from a server SDK only has no users and `crash_free_users: null`.

**Safety of sessions.** The DSN of a browser app is public, so anyone can send sessions and lower a release's crash-free rate. Davy bounds what that costs:

- At most 1,000,000 started sessions per project and UTC day, summed over `release_sessions`. Past that, new sessions are refused and counted in `client_reports` as `davy_daily_cap` with category `session`. Updates to sessions already counted, an error or a crash, still land.
- At most 100 release and environment pairs per project and hour, so made-up release names cannot grow the table without end. A pair that exists keeps counting.
- At most 100 counts per `sessions` item and 1,000,000 per count, and a release of at most 200 printable characters, an environment of at most 64.
- `session_seen` holds one row per session counted and drops it 5 days after the session started, so a project holds at most about 6 million rows, the daily cap over the window.

What it cannot stop is someone who sends crashes for a release that exists. The counts are what SDKs claimed, as with client reports.

## Cron check-ins

A cron job instrumented with a Sentry SDK (`Sentry.withMonitor`, `Sentry.captureCheckIn`) sends `check_in` envelope items. Davy maps them onto heartbeat monitors, so the job gets the same incidents and alerts as a job that pings `/api/heartbeat/<token>`. The reference is Sentry's docs (develop.sentry.dev, Check-Ins; docs.sentry.io, Crons). The fixtures in `e2e/fixtures/sentry-node-10-checkins/` and `sentry-node-7-checkins/` are what the SDKs send.

### Wire protocol (what we accept)

```json
{"check_in_id": "570f34923dae4c8cbd32c19651a1e363", "monitor_slug": "nightly-report", "status": "in_progress",
 "release": "jobs@1.2.0", "environment": "production",
 "monitor_config": {"schedule": {"type": "crontab", "value": "0 * * * *"}, "checkin_margin": 2, "max_runtime": 10, "timezone": "Europe/Prague"}}
{"check_in_id": "570f34923dae4c8cbd32c19651a1e363", "monitor_slug": "nightly-report", "status": "ok", "duration": 0.0000236}
```

- `status` is `in_progress`, `ok` or `error`. `duration` is in seconds. `schedule` is a crontab of 5 fields or `{"type": "interval", "value": 2, "unit": "hour"}` with unit `minute`, `hour`, `day`, `week`, `month` or `year`. `checkin_margin` and `max_runtime` are minutes. `timezone` is an IANA name.
- `release`, `environment`, `contexts` and the alert thresholds (`failure_issue_threshold`, `recovery_threshold`) are ignored. One monitor per slug serves every environment.
- An all-zero, missing or malformed `check_in_id` means "the open run, whichever it is".

### Data shapes

```rust
struct CheckIn { slug: MonitorSlug, status: Status, id: Option<CheckInId>, duration: Option<Duration>, config: Config }
enum Status { InProgress, Ok, Error }
enum Config { Absent, Valid(MonitorConfig), Invalid }          // Invalid still pings a known monitor
struct MonitorConfig { schedule: Schedule, margin: Duration, max_runtime: Duration }

// The one shape a heartbeat's expectation can take. Stored as every_s, or months and zone, or cron and zone.
enum Schedule {
    Every(Duration),                                     // minute, hour, day, week intervals, and CLI heartbeats
    Months { n: u32, zone: Zone },                       // month and year intervals, calendar months in the zone
    Cron { spec: Crontab, zone: Zone },
}
enum Source { Cli, Sdk }                                 // who owns the monitor's settings
```

`Crontab` is a parsed 5-field expression: minute, hour, day of month, month, day of week. Each field is `*`, a number, a range `a-b`, a step `*/n` or `a-b/n`, or a comma list of those. Months take `JAN`..`DEC`, days `SUN`..`SAT`, and day 7 is Sunday like 0. `@yearly`, `@annually`, `@monthly`, `@weekly`, `@daily`, `@midnight` and `@hourly` stand for their expressions. When both day fields are restricted, a day matches either one, as in Vixie cron. An expression that can never match, such as `0 0 31 2 *`, is refused when it is parsed. The parser is pure and the schedule computes times without a clock: `Schedule::next(after)` is the first scheduled instant after `after`.

A crontab time that a DST change skips runs at the same offset as before the change (02:30 on the spring day in Europe/Prague is 03:30 CEST), and a time that happens twice runs once, at the first. That is what jiff calls the compatible conversion and what most cron daemons do.

### Monitors from check-ins

- A check-in for a slug that the project has no monitor for creates one when it carries a valid `monitor_config`: a heartbeat monitor named after the slug, owned by `Source::Sdk`, with no token. A later check-in with a config updates the schedule, margin and maximum runtime of an `Sdk` monitor.
- `davy monitor add <project> <name> --heartbeat --slug <slug>` makes a `Cli` monitor that takes check-ins for that slug. Check-ins ping it but never change it: its expectation stays `--every` and `--grace`, and a `monitor_config` is ignored. An operator who wants senders unable to change a monitor makes it this way.
- `ok` is a ping, `error` a failure ping, as on the heartbeat route. `in_progress` starts a run and records no check, so it neither raises nor resolves an incident.
- The expectation after an `ok` or `error` at time `t`: `Every(d)` expects the next by `t + d + margin`, `Months` by `t` plus `n` calendar months in the zone plus margin, and `Cron` by `next(t + margin) + margin`. The `+ margin` inside `next` makes a run that starts up to `margin` early count for that scheduled time, not the one after it. A `Cli` monitor uses its grace as the margin.
- A run started with `in_progress` must end with `ok` or `error` within `max_runtime`. An `ok` or `error` ends the run when its `check_in_id` is the run's or is absent. The SDKs send a run's `in_progress` and its end at the same moment, so for a short job the end can arrive first: an `in_progress` whose `check_in_id` names the last run that ended is ignored, or it would open a run that never ends. When the run's time is up first, the sweep opens an incident with the reason `run timed out after <n> min`. When the next check-in is not there by the expected time, the reason is `missed check-in`. A `Cli` heartbeat keeps `no ping in time`. Each opens the incident at once, like a late heartbeat, and the next `ok` resolves it.
- A monitor that has never had a check-in is `Pending` and expects nothing, as a heartbeat does before its first ping. An `in_progress` as the first check-in starts the run clock, so a first run that never ends is noticed. The one race this leaves: when a new monitor's first run ends before its `in_progress` arrives, the end has no monitor yet and is dropped as `davy_unknown_monitor`, and the run times out once. A job that runs longer than one request to Davy does not hit it.
- A monitor does not go down twice for one miss: after the sweep opens the incident, the monitor stays down without a new expectation until the next check-in.
- A failure check-in (`error`) with a duration stores it as the check's `ms`. An `ok` without a duration takes the time since its run started.

### Bounds

The DSN is public, so anyone who has it can send check-ins. Every count and size has a limit, and every refused check-in is counted in `client_reports` (see [What else the SDKs send](#what-else-the-sdks-send)) with the category `monitor` and a reason from the table below, so what the SDKs dropped and what Davy refused are in one table that `davy sql` and the `sql` MCP tool read. Nothing is logged per drop, so a flood cannot fill the log. The config is read as a JSON tree, and the request's memory budget pays for that tree while it is read, as it does for an event.

| Input | Limit | Over the limit (`reason`) |
|---|---|---|
| Check-in item | 100 KiB (Sentry's limit) | dropped (`davy_too_large`) |
| Check-in items per envelope | 1, the first | later ones are ignored, like extra events |
| Item that is not a check-in object, or has no valid status | | dropped (`davy_invalid`) |
| Slug | 1 to 50 characters of `A-Z a-z 0-9 _ -`, stored in lower case | dropped (`davy_bad_slug`) |
| Unknown slug without a valid config | | dropped (`davy_unknown_monitor`, or `davy_bad_config` when a config came but was invalid) |
| Monitors created from check-ins | 100 per project | dropped (`davy_monitor_cap`) |
| Interval | 1 minute to 1 year (12 months) | config invalid |
| Crontab | 5 fields, at most 200 bytes, values in range | config invalid |
| Time zone | an IANA name in the bundled database, at most 64 bytes; absent is UTC | config invalid |
| `checkin_margin` | 0 to 1,440 minutes, default 1 | clamped |
| `max_runtime` | 1 to 10,080 minutes (7 days, Sentry's cap), default 30 | clamped |
| `duration` | 0 to 7 days | the duration is ignored |
| All check-ins per monitor, including `in_progress` | 6 a minute (Sentry's limit) | dropped (`davy_rate`) |

Starts share the six-per-minute budget with completions. A CLI monitor ignores starts.
A repeated start never resets an open run's clock. SDK config updates are
authorized by the public DSN: a sender can silence a job by changing its
schedule. The monitor list exposes `config_changes`, counting actual config
changes after creation. Use a CLI monitor to fix the schedule, and omit
`--slug` for token-only access.

Unreadable stored schedules are flagged as `invalid stored schedule` in
the monitor list and their check-ins are dropped as `davy_bad_config`.
They do not break listing or check-ins for other monitors.

Resolved incidents and sent or permanently failed notifications expire after
30 days, matching checks retention. Open incidents and incidents overlapping
the 30-day uptime window remain. Starts expire after one minute. Each sweep
deletes in rounds of up to 1,000 rows from each class (checks, incidents,
sent notifications, failed notifications, starts) until no class has a full
batch left or 100 ms are spent. The first round always runs and the classes
take turns, so a backlog in one class cannot starve another. The sweep runs
in the writer every 10 s, so 100 ms is at most 1% of its time; the last
round can overrun the budget by a few milliseconds.
A sweep that deleted one batch per class could not keep up once about ten
projects flooded check-ins, because a public DSN can create monitors and
incidents and the 1,000-monitor limit does not cover them. Measured on an
M2 Max under a load average of 13 (release build of a scratch program, a
file database with 1,000 monitors and 1,400,000 expired rows): one batch per
class deleted 3,000 rows in 27 ms; a budgeted sweep deleted 57,000 to 60,000
rows in about 130 ms including its commit, 261,000 rows/s while sweeping.
That is about 19,000 rows per class per sweep where 1,000 were before, so a
flood needs about 19 times as many projects to outgrow it.
Incidents are not capped per day: dropping spans would inflate uptime.
The six-per-minute check-in budget bounds their creation instead.
Monitors created by check-ins do not count toward the 1,000 monitors of
`davy monitor add`, so a sender cannot use up the room an operator needs.

### Storage

Migration 12 adds to `monitors`: `slug` (unique per project when set), `source` (`cli` or `sdk`, existing rows `cli`), `months`, `cron`, `zone`, `max_runtime_s`, `run_id`, `run_started_at`, `ended_run_id` and `due_at`. A `CHECK` makes the schedule one of the three forms: `every_s > 0` with neither `cron` nor `months`, or `months` with a zone, or `cron` with a zone. `due_at` is the instant the sweep compares with the clock, for every heartbeat: a ping sets it, so the sweep is one indexed comparison and the schedule is computed once per check-in, not per sweep. Existing heartbeats get it from their last ping.

Migration 13 adds `channels.sdk_dropped`, the notices of monitors made by check-ins that a full outbox dropped. `dropped` keeps the rest, and the drops counted before the upgrade stay there.

### Time zones

A zone name from a sender goes through jiff's bundled copy of the IANA database (`tzdb-bundle-always`), not the host's `/usr/share/zoneinfo`. The binary is static and may run in a container without zoneinfo, and the same name must mean the same rules on every host. The bundle adds about 230 KB: 207 KB of zone data and the table of names.

### Not in this feature

- The HTTP check-in route (`/api/<project>/cron/<slug>/<key>/`). The SDKs send envelopes, and a shell script can ping a heartbeat. It adds a route with its own credential tests, so it waits for someone who asks.
- `--cron` for `davy monitor add`. A `Cli` monitor expects a ping every `--every`.
- Failure and recovery thresholds, monitor owners and per-environment monitors.

## Retention per project

An operator, or an agent over MCP, sets how long each project keeps its logs and its errors. A project without a setting of its own follows the server.

### Settings

- `davy serve --log-retention-days <n>` is the server default for logs, 30 as before. `--error-retention-days <n>` is the default for errors, 36,500 (100 years), so an agent can still ask about any error ever seen, and nothing stored before is deleted without a setting that says so. Both take 1 to 36,500. Lowering a default deletes, for good, what is older in every project that follows it: logs within a minute, errors within 10 s. A project that must keep its data whatever the default says needs a setting of its own.
- Migration 14 adds `projects.log_retention_days` and `projects.error_retention_days`, NULL for the server default, each with `CHECK(... BETWEEN 1 AND 36500)`, so a hand-written `UPDATE` cannot store a value the sweep would misread either.
- `davy project retention SLUG --data DATA [--logs N|default] [--errors N|default]` changes them and prints the project's own settings as JSON, `{"errors_days":null,"logs_days":7,"project":"api"}`, null where it follows the server. The CLI cannot know the defaults of a running server, so it prints only what is stored. With no flag it only prints.
- `POST /ui/api/retention` takes `{"project":"api","logs_days":7,"errors_days":"default"}` and answers with what the project keeps now. `GET /ui/api/retention` lists every project. A setting left out stays as it was. A number outside 1 to 36,500 or a word other than `default` is `400` with the reason, an unknown project `404`.
- The MCP tool `set_retention` takes the same fields but may only keep data longer. A setting shorter than what the project keeps now refuses the whole change and names the CLI and the web UI. So does `"default"` for a project with a setting of its own, even when the default is longer today: an operator can lower the default later, and the project would lose the floor a person set. `"default"` for a project that follows the default already changes nothing. Shortening deletes data within a minute and for good, and an agent's prompt can carry text from someone else's error or log line, so only a person shortens. Every stored change prints one line to stderr, `retention: an agent set project "api" to logs 90 days, errors the default`, so the journal shows who changed what. `project_status` shows `retention`: `{project, logs, errors}`, each `{days, server_default, deletes_before}`, with `deletes_before` in RFC 3339.
- A change goes through the writer queue as `Job::SetRetention`, like every other write of the server. The next sweep applies it: within 10 s for errors, within a minute for logs. The writer holds `retention::Lock` while it writes a batch that holds a change, until the batch is committed, and the log writer holds the same lock while it reads the settings and deletes from a day, so a change the server reports as saved is never followed by a deletion under the setting it replaced. The errors read the setting inside the delete statement on the same writer, so they need no lock.
- The CLI writes to the same database from a process of its own and cannot take that lock. A CLI change that meets a running server's log pass can still lose the logs of the one day being deleted from at that moment under the old setting. Change a setting through the web UI while the server runs, or stop it first.

### Errors

The SQLite writer runs `Job::Sweep` every 10 s, and the errors take their turn in it next to the monitors, with a budget of their own of 100 ms (`retention::errors::BUDGET`). Each turn deletes one batch of expired events, then at most 200 expired user feedback rows, then unlinks at most 50 user feedback rows from issues whose last event expired, until none is left or the budget is spent. A batch of events stops at 200 events or at about 10 ms of work, whichever comes first (`WORK`). An event costs one unit, about 6 µs, and one more per 4 KiB of payload, which SQLite frees page by page. The first event of each issue in the batch costs 50 more, about 0.3 ms, for the statements that keep the issue's counts and tags. Both costs come from the measurements below. A batch takes at least one event, so a sweep ends at most about 10 ms past its budget, plus the cost of one event with a large payload, however the events spread over issues. A batch of 200 events in 200 issues used to take about 60 ms. SQLite rewrites a whole row to unlink it, and 50 rows with every field at its limit are about 1.3 MB, about 320 pages, so an unlink batch is estimated at about a fifth of a batch of events. That estimate is not measured.

- An event expires by `received_at`, the time Davy stored it, so a client clock cannot keep an event or expire it early. The new index `events(project_id, received_at)` makes each project's expired events one range. User feedback expires by its own `received_at`.
- Deleting an event takes it out of its issue's `event_count` and `issue_hourly`, and sets `first_seen` and `last_seen` to the oldest and newest timestamp of the events left, as ingest keeps them: the smallest and largest timestamp, whatever order the events arrived in. A client clock can be ahead, so the event that expires first can hold the newest timestamp. Migration 16 adds the index `events(issue_id, timestamp)`, so each is one index lookup. It runs in the one transaction of the upgrade, after the check that no newer Davy migrated the store. Building it reads every event once, as long as the index on `events` of migration 14 did, and needs as much free disk.
- `issue_tags` gained `last_at`, when an event with that environment or release last arrived, and an index by issue. The migration backfills it from the events. A tag goes when its `last_at` is no later than the newest deleted event that carried it and no event left that arrived in that same millisecond carries it, so a filter by environment or release finds only issues that still have such an event. A project's events are deleted in the order they arrived, so an event left that carries the tag arrived after the newest one deleted, which makes `last_at` later, or in the same millisecond, as the events of one request do, which the index by project and arrival finds.
- **An issue whose last event expired is deleted**, with its hourly counts and tags. Its user feedback stays, unlinked, until it expires too. An issue can hold 1,000 feedback rows a day, so they are unlinked in batches of 50 in the sweep's turns, and the issue keeps an `event_count` of 0 until the last is unlinked. Migration 17 adds the partial index `issues(id) WHERE event_count=0`, so finding those issues reads no other issue. If the error returns first, the issue counts its new event and keeps the feedback still linked. Keeping an empty issue would show a count of 0 and a first seen time with nothing behind it, and agents read counts as facts. When the error returns it is a new issue: unresolved, with a new-issue alert, because the resolved or ignored state went with the old one.
- **An issue id is never used twice.** SQLite gives a new row the highest id plus one, so deleting the newest issue would hand its id to the next one, and a link in an old alert or an agent's notes would lead to an unrelated issue. The new table `id_floor(name, id)` keeps the highest issue id ever deleted, and a new issue takes one more than the larger of that and the highest id in `issues`. AUTOINCREMENT would do the same but needs the table rebuilt.
- A row of another table that still points at an issue keeps it with an `event_count` of 0, rather than fail the writer's batch, which holds ingest too.
- `releases` stays: it says when a release was first seen, which decides whether a resolved issue reopens. Release health counts sessions, not events, and keeps its own 90 days.
- A lower `event_count` moves which later events the payload sample keeps, so the sample still keeps about one in 100. It does not clear a payload that is in the sample: a new event clears only a payload marked as kept for being the newest.
- Measured on an M2 Max, release build, SQLite's default 2 MiB page cache, 300,000 events over 100 days and a retention of 10 days, so 270,000 expire: with every event in one issue a sweep deleted about 18,000 events in its 100 ms and all were gone after 15 sweeps. With the events spread round robin over 1,000 issues, the worst case, a sweep deleted about 870 events, the longest sweep took 142 ms and its commit 50 ms more, and the backlog took 311 sweeps. Each issue a batch touches costs about 0.3 ms of page reads. With a 128 MiB page cache the spread case ran three times as fast. These runs used batches of 200 events, before the work bound, which has not been measured again. The migration's backfill took 360 ms over the 300,000 events.

### Logs

All projects share one Tantivy index per UTC day, so a project's own retention cannot delete a directory.

- A day older than every retention, default and projects, is renamed into the trash whole, as before. While no project has a setting of its own, that is all retention does.
- A younger day loses the logs of each project whose retention it outlived: one `delete_query` for those projects, or for every project but those that keep it when the day outlived the default, then a commit, then a merge of exactly the segments that hold deletes, so the logs leave the disk and not only the results. A day left with no logs is deleted. The log writer closes that day's own writer first, because an index has one writer at a time.
- The log writer reads the projects' settings from SQLite at start, before the first batch, and every minute after, then runs the pass. Before it deletes from a day it takes `retention::Lock`, reads the settings again and works out the day's cut anew, and it lets go once the deletion is committed, before the merge. A pass that cannot read the settings, at its start or before a day, deletes nothing more and reports it on stderr. Reading no settings as "no project has one" would delete every day older than the default, the logs of projects that keep them longer included. It remembers what it cut from each day, so a day is read again only when a setting changes or the day outlives another retention. A pass that has cut or removed days for longer than 1 s stops after that day, and the next goes on. A day that fails, an index that does not open for instance, is reported on stderr and skipped, so one broken day cannot stop the retention of every day after it. Each pass first empties `.trash` of what a crash left between renaming a day there and removing it.
- Ingest waits while a day is cut, and the queue of 50,000 records absorbs that. Measured on an M2 Max: cutting half of a day of about 1,000,000 logs (176 MB) took 4.4 s, and a day of 120 MB 3.4 s. A shipper that sends more than the queue holds in that time gets `429` and retries. This happens once per day and project with a shorter retention, and after a change.
- A log that arrives late is judged by its own project's retention: in a project that keeps 7 days, a log stamped 10 days ago takes the time it arrived and keeps the original in `davy.original_timestamp`, as every project did under the server default, so it is not deleted the moment it is written.

## Hardening

A security review of `370e4cd` showed that a client without a key could make the server allocate 4.4 GB with one 194 KB request (measured with a release build on a developer laptop: a gzip body that inflates to 190 MiB of `[0,0,...]` sent to `/store/`). The rules below close that class. Each has a test that a request over the limit is refused.

### Authentication before work

- A Sentry request with the key in `X-Sentry-Auth` or `?sentry_key=` is checked before the body is read.
- `/store/` has no envelope header, so a `/store/` request without a header or query key is a `401` before the body is read.
- An envelope without a header or query key is read only until its first line decompresses: at most 128 KiB of the body, plus the frame that crosses that mark. Davy decompresses at most 64 KiB of that line, parses it as the envelope header and checks the key in its `dsn` against the project in the path. Only then does it read the rest. A header line over 64 KiB, or none in the first 128 KiB, is a `400`. Each try decompresses from the start, so a new try waits until the body has doubled, and a body sent a byte at a time costs a few tries, not one per byte.
- Logtail and OTLP check the bearer token before reading the body, as before.

### Sizes and counts

Sentry's own limits (develop.sentry.dev, Envelopes, Size Limits) are 200 MiB for a decompressed envelope and 1 MiB for an event or log item. Davy keeps the 1 MiB item limit, so an item that Sentry accepts fits. It caps a decompressed body at 64 MiB, because it keeps events and logs and drops attachments, the items that need Sentry's room.

| Input | Limit | Over the limit |
|---|---|---|
| Request body, compressed | 20 MiB | `413` |
| Request body, decompressed | 64 MiB | `413` |
| Memory one request holds while it is read and parsed | 256 MiB, shared by all requests | `413` when one request needs more, `429` when others hold it |
| Log records per request | 10,000 | `413` |
| Event item, and a `/store/` body | 1 MiB | the item is dropped, like any dropped item type |
| Log item in an envelope | 1 MiB | the item is dropped |
| Items per envelope | 1,000 | `400` |
| Event items per envelope | 1 (develop.sentry.dev, Envelope Items) | later event items are dropped |
| Release and environment of an event | 200 and 64 characters, as Sentry takes them | the event is kept without it, so it has no release or environment tag |
| Client report, feedback and session items per envelope | 10 that parse | the rest are dropped |
| Client report entries | 100 per report, 1,000,000 per entry, 100 reason and category pairs per project and day | later entries are dropped, a larger quantity counts as 1,000,000, a new pair is not counted |
| Sessions | 1,000,000 started per project and UTC day, 100 release and environment pairs per project and hour, 100 counts per `sessions` item, 1,000,000 per count, started within the last 5 days and at most 1 h ahead | counted in `client_reports` as `davy_daily_cap` or `davy_out_of_window`, a larger count counts as 1,000,000 |
| User feedback | 1,000 per project and UTC day, message 4,096 characters, name 128, e-mail 256, URL 2,048 | counted in `client_reports` as `davy_daily_cap`, longer text is cut |
| Frames symbolicated per event | 256, from the frame that threw outward | older frames stay minified |
| Source line kept per context line | 256 bytes around the column | the rest is cut, marked `…` |
| Log message | 64 KiB | cut at a character boundary, attribute `davy.truncated` set to `true` |
| Log queue | 50,000 rows or 128 MiB of record heap, held until written | `429`, or `413` for a batch larger than the queue |
| `payload_json()` output in `sql` | 16 MiB | the query fails with an error |
| Files in an artifact bundle | 10,000, and 1 GiB unpacked in total | assemble answers `error` |
| Parts in a chunk upload | 4, the announced `chunksPerRequest` | `400` |
| Chunk, decompressed | 8 MiB, the announced `chunkSize` | `400` |
| Chunks in an assemble request | 32, a 256 MiB bundle in 8 MiB chunks | `400` |
| Memory to parse one build, and the cache of parsed builds | 256 MiB each, as priced before parsing | assemble answers `error`, or the build is not parsed and its frames stay minified |

### Time and concurrency

- Requests pay for the memory of their bodies from one budget of 256 MiB before they use it: the bytes received, the decoder's window and tables, the room the decompressed body grows into, the tree a parse builds and the buffer it unescapes strings into, what symbolication adds to an event, and each log record. A decoder is priced by its encoding: 128 KiB for deflate, 384 KiB for gzip, whose header can hold three fields of 64 KiB, 9 MiB for zstd, and for brotli the window its stream asks for plus 6 MiB of Huffman tables. Decoding runs on the blocking pool of up to 512 threads, so an unpaid window of 16 MiB could have held 8 GiB. A tree is priced from the body before it is built (`src/heap.rs`), because memory follows the values, not the bytes: `[{},{},...]` costs over a hundred heap bytes for every three bytes of input, and an OTLP record copies the attributes of its resource. A queued event keeps what it paid until the writer has stored it, even when its client hangs up. A request that needs more than the whole budget gets `413`. One that finds the budget held by others gets `429` with `Retry-After: 1`. The 0.1.0 pentest found that the 8 parsing slots this replaces did not bound what each body parsed into. The test `a_small_body_of_many_values_is_413_before_it_is_parsed` sends a gzip body under 4 KB that would parse into 100,000 records.
- A slow client holds what it has sent until it finishes or times out. A client without a key holds at most 128 KiB of an envelope and its decoder while Davy looks for the key in the header line, and a client with a public key can send whole bodies. Either can hold the budget with slow bodies, and others get `429` for up to 30 seconds. Limits per client address are the reverse proxy's job.
- Reading a request body has 30 seconds. A slower client gets `408`. Slow headers are the reverse proxy's job, because SECURITY.md already requires one for TLS.

### Other inputs

- A `trace_id` that is not 32 hex characters is ignored, so it cannot break the log query in `issue_context`.
- The CLI replaces control characters other than newline and tab in titles, messages and attributes with `U+FFFD` before printing to a terminal. `--json` output is unchanged, because JSON already escapes them.
- The admin token is read only from `DAVY_TOKEN`. A token on the command line shows up in `ps` and shell history.
- The SMTP URL is read only from `DAVY_SMTP_URL`, for the same reason, because it holds the SMTP password.

### Before the cloud launch

These are fine for one team on its own server and must change before tenants share one:

- **Source maps per tenant.** Builds are stored by debug ID only. One tenant could upload a map under another tenant's public debug ID and rewrite its stack traces. The store key and the lookup gain the project.
- **Retention for errors.** Events, issues and hourly counts are kept 100 years unless a project or `--error-retention-days` says otherwise ("Retention per project"). A sender with a key can open a new issue per event. A cap on new issues per project per hour and a retention each tenant cannot raise come with the cloud quotas.
- **Abandoned chunks.** Chunks of an upload that never assembles stay in `data/chunks/`. A sweep removes chunks older than an hour.
- **Tenant scope in agent tools.** `sql` and `mark_fixed` see every project in the database.
- **E-mail recipients.** An e-mail channel mails any address. The cloud verifies an address with a confirmation link before a channel sends to it.

## License and open core

- **Open, AGPL-3.0 (this repo).** The single binary someone runs themselves: Sentry, Logtail and OTLP ingest, issues, log search, uptime, alerts by e-mail, webhook and Slack, CLI, MCP, and a basic UI for one team.
- **Closed, in a private `davy-cloud` repo that depends on this crate.** The managed cloud: what it takes to run Davy for many teams at once.
- A contribution's author agrees that the owner may also license it under other terms, so the core can also be sold under a commercial license. Today that is a checkbox in the pull request template (`CONTRIBUTING.md`, "What to send"). A formal CLA may replace it before 1.0.
- Keep the core usable as a library: the cloud must not need a fork.

## Log storage (decided by benchmark)

Logs go into Tantivy, one index per day, documents stored zstd-compressed. At 10M rows it took 0.67× the raw size on disk against 1.80× for SQLite FTS5, and counted by level in 4.2 ms against 98 ms. Numbers, method and tradeoffs are in `docs/storage-benchmark.md`.

## Connect a new app

`davy connect SLUG --data DATA [--sdk SDK] [--local]` prints a JSON object
with `project: {id, slug, dsn}`, `supported_sdks`, and, when an SDK is named,
`sdk: {package, install, init, dsn_location}` and `test_event` (a POSIX shell
command posting an error without an SDK). Retrying reuses the project key.
SDK names are android, rust, go, node, python, dotnet and browser.
`project list --data DATA` prints JSON objects with id and slug.

Public DSNs use setup.json's public URL. Local DSNs use HTTP and the saved
listener's port and address family: wildcard binds become loopback; a
non-loopback specific bind is refused because it does not listen on loopback.
The CLI requires saved settings; serve flag overrides must also be saved with
setup before using CLI connect. MCP uses the running server's public URL.

MCP keeps its existing authority: `connect_project(slug, sdk?)` only reads an
existing project. A missing project names the local CLI command to create it.
It cannot mint ingest keys. `project_status(slug)` returns `event_count`,
`last_event_time`, `issue_count` and `last_log_time`, with RFC 3339 times
that are null before the first record. The event count and time come from
the issues index, which holds `event_count` and `last_seen`, and count the
events the project still keeps. Logs become visible after the log writer
commits. The two tools brought the MCP tool count to ten, and
`set_retention` to eleven. `retention` in `project_status` shows the days
of logs and errors in effect and the instant before which they are deleted. `scrub` shows the
project's scrubbing settings.

## Server-side data scrubbing

Every project defaults to scrubbing sensitive keys, credential patterns,
credit cards (13 to 19 digits starting with 2 to 6, validated with Luhn)
and standalone IP addresses, excluding loopback and unspecified addresses.
E-mail and personal identity scrubbing default to off: applications set
user IDs, e-mail and feedback contacts deliberately for affected-user
counts and replies. Migration 15
adds `projects.scrub`, a JSON settings object; existing data is not rewritten.
The fields are `keys`, `credentials`, `cards`, `email`, `ip`, `personal` (booleans,
default true except `email` and `personal`) and `extra_keys` (at most 100 names of 100 bytes each).
Sensitive keys retain their name and get `[Filtered: sensitive]`. Strings
get fixed class markers, without a hash or a suffix from the removed value.

Events are scrubbed after symbolication and before typed parsing, grouping,
queueing, SQLite and issue search. Thus removed values cannot split issues.
Logs from every adapter are scrubbed once before the log queue and Tantivy.
SDK metadata, check-ins and feedback are scrubbed before conversion; MCP feedback uses
the safe defaults. Attachments, replays and unknown envelope items are dropped
without persistence. Uploaded source maps are trusted admin build artifacts,
not ingest attachments, and are outside this policy.

`davy project scrub SLUG --data DATA` prints settings; `--settings JSON`
replaces them, and fields it leaves out take the defaults above. Admin
bearer-authenticated GET and PUT `/api/projects/{project}/scrub` read and
replace the same object. Both refuse unknown fields, more than 100 extra
keys, keys over 100 bytes, keys with no ASCII letter or digit, which
would normalize to nothing, and keys with `%` or `+`, as a form-encoded key
in text is decoded before it is compared, so the decoded name is the one to
give. A stored policy is read leniently, so one
written by a newer version still applies. A PUT for a project that does not
exist answers `404` before it reaches the writer, and the writer fails only
that job, so other writes in its batch commit.

The API only tightens. An agent holds the admin token, and an event can
prompt-inject an agent, so a PUT must name every field and may not turn a
class off or remove an extra key. A weakening PUT answers `403` with what it
would weaken and the `davy project scrub` command that does it; the CLI
needs the data directory and may set anything. Changes are serialized, so
two tightening PUTs cannot together weaken. Every change is logged to
stderr at warning level, with the old and new policy and whether the CLI,
the API or an edit outside the API made it.

Enable identity filtering with `--settings '{"email":true,"personal":true}'`
or PUT the full policy with those fields to the admin API. `personal` covers
identity fields in `user` and feedback names, never OS, browser, runtime or
SDK names. `email` covers contact fields and e-mail patterns in text.
Keys are compared in ASCII. Before that, look-alike letters fold to the
Latin letter they look like: NFKD turns fullwidth and styled letters into
ASCII and drops accents, a fixed table maps Cyrillic and Greek letters
such as `о` and `ο`, and soft hyphens, zero-width and other invisible
format characters drop. So `passwоrd` with a Cyrillic `о`, `ｐａｓｓｗｏｒｄ`
and `pass\u200bword` are all `password`. Any other non-ASCII character
separates words. Extra keys fold the same way when stored, and apply
whether or not the `keys` class is on. A key longer than 256 bytes is
checked by its first and last 256 bytes. Each window is folded whole: a
run of separators folds to one, so no character takes more than twice its
bytes (`㎯` is `rad-s2`), and a character that decomposes into many letters
cannot push a root out of the window.
Sensitive keys split on separators and camelCase. Distinctive roots
(password, passphrase, secret, credential, accountkey, sharedaccesskey and
similar) match normalized substrings; short roots (auth, token, session,
pwd, ssn, cvv, cvc, signature) must end a word, and pin and otp must be a
whole word, so `spinner` and `shipping` stay. In a name/value pair, as in
HTTP headers, a sensitive `key`, `name`, `Name` or `Key` hides the sibling
`value` or `Value`. Key matches replace strings recursively and preserve
numbers, booleans and null; card detection also checks integer values. An
object key that holds a credential, a card, an IP address or (when `email`
is on) an e-mail address is renamed to its marker, and keys that collide get
a counter: `[Filtered: email]`, `[Filtered: email] 2`.

Structural fields skip the e-mail, IP address and card patterns, because
they identify, group or route a record: a release `1.2.3.4` is not an
address and a release `shop@1.2.3` is not an e-mail address. They are
`release`, `dist`, `environment`, `platform`, `level`, `logger`, `event_id`,
`trace_id`, `span_id`, `parent_span_id`, `monitor_slug`, `check_in_id`, and
in sessions `did` and `sid`, which release health counts users by.
Sensitive assignments and credential patterns still apply to them, so
`"release":"password=x"` or a token in `logger` is removed. A name is
structural only where the item keeps its own fields: at the top level of
an item or a log record's attributes, in an event's `contexts.trace`, in a
session's `attrs` and in each entry of session `aggregates`. Anywhere else,
as in `extra`, `tags`, breadcrumb data or other contexts, the same name is
free text and gets every pattern. A sensitive key under a structural name
is still hidden. IP candidates must have no ASCII letter, digit, underscore,
dot or colon on either side, so `Foo::bar` and
`v1.2.3.4.5` stay. A dot that ends a sentence is allowed, and an IPv4
address with a port (`10.0.0.5:5432`) is removed whole. Letters next to a
candidate count only in ASCII, as in the card and credential patterns, so
`é192.0.2.1` is found, and so is a version written right after Chinese or
Japanese text (`版本2.11.0.7`), which is removed as an address.
`project_status` exposes the current policy over MCP. Ingest holds each
project's policy for 2 seconds; a PUT replaces it at once, and a CLI edit
applies within 2 seconds without a restart. Disabling a class permits that
data to be stored in future records; it does not restore removed data.

Patterns use Rust regex's finite automata with fixed patterns; no configured
regular expressions are accepted. Each pass is linear in the string size,
and extra keys are compared through one hash lookup per key. Word
boundaries in the credential and card patterns are ASCII, so a card right
after a non-ASCII letter (`č4242…`) is still found, and non-ASCII text
keeps the fast automaton instead of the slower one Unicode boundaries need.
Text that
looks like JSON, form data or query parameters is inspected for sensitive
assignments, including JSON escaped inside a string (`{\"password\":\"..\"}`);
text without `=`, `:` or `%3` skips that pass. An assignment key may hold
non-ASCII letters, which fold as object keys do. A form-encoded key decodes
up to 3 times, so `p%2561ssword` (encoded twice) is `password`. A quoted
value ends at its closing quote; an unquoted one runs to the next `&`, `;`, `,`, `}`, `]` or
line break, so `password=a b c` hides all of `a b c`. A string with more than
256 sensitive assignments becomes `[Filtered: too many assignments]`. Opaque
request body strings and fields named as attachments are removed in full
when sensitive-key filtering is enabled. Log ingest checks the 10,000 record
limit before it scrubs.

Markers can be longer than what they replace, by more than 4 times for a
short credential such as `ghp_x`. Scrubbing pays for every allocation from
the request's memory budget before it makes it, in whole KiB, as parsing
does: twice for events and SDK items, whose tree is priced twice, once for
log records. A payment the budget refuses stops the scrub, and the request
answers `413` or `429` as an over-budget request does today; what was paid
and not used goes back when scrubbing ends. `tests/heap.rs` checks that
what a scrub pays bounds its peak heap. The regex automata and their
per-thread caches are allocated once and are not charged.

Measured on an Apple M2 Max, release build, with `cargo run --release
--example scrub_cost`, best of 5 runs on a shared machine: the worst input
found costs about 53 ms of CPU per MiB of JSON (1 MiB of `a:` with 100 extra
keys of 100 bytes; 48 ms with the default policy). 1 MiB of `ﷺ:`, a key
character that NFKD expands to 18 letters, costs 45 ms, 1,000 keys of 1 KB
that fold to twice their size cost 12 ms, and renaming 60,000 IP-address
keys takes 43 ms for 0.97 MB. A typical 2.8 KB SDK event
costs about 36 microseconds more with scrubbing than without. So a request
costs at most about 60 ms per MiB the memory budget lets it decode.

This is a conservative filter, not a detector of arbitrary encodings or
natural-language personal information. Known gaps: a URL password that
contains `/` (`https://u:a/b@host`), card numbers grouped with dots, an
assignment whose key holds a space (`api key=...`), a key encoded more than
3 times, an assignment with a non-ASCII separator (`password＝x`), a
sensitive name not wholly within the first or last 256 bytes of a longer
key, and look-alike letters outside the fold table pass unchanged. Use
the SDK's `beforeSend` for them.
