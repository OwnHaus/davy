# Davy design

Davy is one self-hosted binary that accepts what the Sentry SDKs and the Better Stack (Logtail) log shippers send. It groups errors into issues, stores logs, checks uptime, and serves all of it to people and to coding agents over MCP.

This document fixes the data shapes and the order of work.

## Principles

- **Wire compatibility over a new SDK.** An app switches by changing its DSN or source token. No code changes.
- **One process, one data directory.** SQLite holds metadata. Bulk data (event payloads, logs) sits behind a store interface, so the engine can change without touching the API.
- **Parse at the boundary.** HTTP handlers turn bytes into typed values (`Envelope`, `ErrorEvent`). Grouping and storage take typed values only and never see raw JSON shapes they have to guess at.
- **Lenient on fields, strict on framing.** A broken envelope or an invalid `event_id` is a 400. A field with an unexpected type or value (`level: "warn"`, a timestamp without a zone, a negative `lineno`) falls back to a default instead, because rejecting it would lose a customer's error.
- **Check the key before the work.** When the key is in the header or the query, it is checked before the body is decompressed, so an anonymous client cannot make Davy inflate 200 MiB.
- **Never fall over under a burst.** Ingest gets a bounded queue. When it fills, Davy answers 429 and the SDKs back off. One writer thread writes in batches.

## Milestone order

1. Storage benchmark (done, `bench/storage`, see "Log storage" below).
2. Errors: Sentry envelope and `/store/` ingest, grouping, issues, CLI to list them (done).
3. **Logs**: Logtail endpoint (msgpack and JSON), the Sentry `log` item, OTLP/HTTP logs, full-text search. *Next.*
4. MCP and CLI for agents: `sql`, `schema`, `issue_context`, `mark_fixed`.
5. Source maps: the `sentry-cli` upload endpoints and debug ID lookup.
6. Uptime checks and alerts.
7. Managed cloud (closed, see "License and open core").

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
    trace_id: Option<String>,      // contexts.trace.trace_id
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

One timestamp function for every source. A number below 1e11 is seconds (fractions allowed), below 1e14 milliseconds, below 1e17 microseconds, else nanoseconds. Fractional numbers are rounded to the microsecond, because an f64 holds about 16 significant digits and the nanosecond digits of a seconds value are noise. A string is RFC 3339, with a naive `YYYY-MM-DDTHH:MM:SS[.f]` or a bare date read as UTC. A msgpack timestamp extension (type -1, 4, 8 or 12 bytes) is decoded per the msgpack spec. `@logtail/pino` sends `dt` as an RFC 3339 string (captured fixture), but other msgpack shippers may encode a date natively. Anything else, or a time older than the retention window or more than 24 h in the future, becomes `observed_at`, and the original value is kept in the attribute `davy.original_timestamp`.

### Adapters

**Logtail.** Content type `application/msgpack` or `application/x-msgpack`, `application/json`, `application/x-ndjson` or `application/ndjson`. Without a content type, a body whose first non-space byte is `[` or `{` is JSON or NDJSON, else msgpack. The body is an array of records, one record, or one record per line. In each record `dt` is the timestamp, `message` the message (a non-string is written as compact JSON), `level` the level. `service`, `trace_id` and `span_id` are lifted out when they are strings, the ids only when they are valid hex. Every other key stays in `attributes` as it came, nested objects included. Decode msgpack with `rmpv`.

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
- One log writer thread owns the open `IndexWriter`s (at most 3 days open, least recently written one closed first, a day idle for 10 minutes closed). It reads batches of `LogRecord` from a queue bounded at 50k rows, counted in rows rather than batches because one batch holds one record or thousands and memory follows the rows. An empty queue takes any batch, so one oversized batch is never refused forever. A full queue is the `429`. A bound of 256 batches let 1M rows in batches of 1000 push RSS to 828 MB (measured). Each writer uses 1 indexing thread and a 64 MB heap (1 thread was faster in the benchmark).
- The writer commits every second when anything changed, and at once when 100k documents are uncommitted. It commits everything on shutdown.
- A failed `add_document` or commit drops that day's `IndexWriter`, and the next write opens a fresh one. Tantivy does not restart its indexing worker after a failed commit, so the next 10k documents would block the writer thread forever and ingest would answer `429` from then on (reproduced in `store_recovers_when_a_day_index_breaks`). The cost is that the uncommitted documents of that day, at most about one second, are lost and logged.
- The HTTP handler answers once the batch is queued, not once it is committed. Error events wait for their commit because they are few and each matters. Logs come in bulk and the shippers retry on failure only. The cost is that a crash loses at most about one second of accepted logs, and a log shows up in search within about one second.
- Retention: `davy serve --log-retention-days <n>`, default 30. Once an hour the writer renames day directories older than that into `data/logs/.trash/` and removes them. It touches only names that parse as `YYYY-MM-DD`.

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

## License and open core

- **Open, AGPL-3.0 (this repo).** The single binary someone runs themselves: Sentry, Logtail and OTLP ingest, issues, log search, uptime, alerts by e-mail, webhook and Slack, CLI, MCP, and a basic UI for one team.
- **Closed, in a private `davy-cloud` repo that depends on this crate.** The managed cloud: what it takes to run Davy for many teams at once.
- Contributions need a CLA, so the core can also be sold under a commercial license.
- Keep the core usable as a library: the cloud must not need a fork.

## Log storage (decided by benchmark)

Logs go into Tantivy, one index per day, documents stored zstd-compressed. At 10M rows it took 0.67× the raw size on disk against 1.80× for SQLite FTS5, and counted by level in 4.2 ms against 98 ms. Numbers, method and tradeoffs are in `docs/storage-benchmark.md`.
