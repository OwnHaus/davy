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
10. Managed cloud (closed, see "License and open core"). Outside users come in only after the cloud's penetration test in `SECURITY.md`.

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
- `429` with `Retry-After` until midnight UTC, at most 300 s, and a `davy_quota` rate limit entry when the project is past a daily quota, or until its bucket takes items again and a `davy_spike` entry when it sends more error events or log records a second than its spike rate for them ([Quotas and spike protection](#quotas-and-spike-protection)).

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
latest_payloads(issue_id INTEGER PRIMARY KEY REFERENCES issues(id), event INTEGER NOT NULL, payload BLOB NOT NULL)
issue_hourly(issue_id INTEGER NOT NULL, hour INTEGER NOT NULL, count INTEGER NOT NULL, PRIMARY KEY(issue_id, hour))
issue_tags(key TEXT NOT NULL, value TEXT NOT NULL, issue_id INTEGER NOT NULL, PRIMARY KEY(key, value, issue_id)) WITHOUT ROWID
issues_text USING fts5(title, culprit, content='issues', tokenize='trigram')
```

`issue_tags` and `issues_text` serve the filters of the web UI's issue list. Triggers keep both in step. Milestone 9's "Load check" says why.

- Timestamps are unix milliseconds.
- `payload` is the zstd-compressed event JSON. `events.payload` keeps it for the first 50 events of an issue, then for every 100th. The newest event keeps it too, so an issue always shows its latest event, but outside the sample its payload goes into `latest_payloads`, one row per issue that the next event replaces, and a newest event in the sample deletes the row. A payload written into the `events` row and cleared by the next event left a hole in a page SQLite had filled and moved past, so a hot issue took as much disk as if it kept every payload, see "Payloads of hot issues" below. The writer compresses the newest event of each issue once per transaction, not every event, so a burst costs no more than before. Always write the `events` row and bump the counters, so counts stay exact while disk stays bounded under a burst.
- A duplicate `event_id` in the same project is ignored (SDKs retry).
- The writer drains the queue in batches and commits each batch in one transaction. The transaction begins IMMEDIATE, taking the write lock before its first read: a CLI that writes from its own process, such as `davy resolve`, then makes the batch wait out the 5 s busy timeout. A deferred transaction that read first cannot wait for a lock it takes later, so SQLite failed the whole batch with `database is locked`.

### Migrations

Every command that opens the store with `store::open` runs the pending migrations first: `davy serve`, `davy connect`, `davy issues`, `davy event`, `davy context`, `davy resolve`, `davy feedback`, `davy logs --project`, the `davy monitor`, `davy channel` and `davy user` commands, and every `davy project` command but `list`. Some of them only read, and still migrate. `davy sql`, `davy project list` and `davy usage` open the database read-only: they never migrate it, and refuse one from an older Davy with a message to run `davy serve` once. `davy doctor` reads it the same way for its disk forecast, which then shows that message. `davy setup`, `davy systemd-unit` and `davy mcp-config` never open it. The migrations run only while no other process has the database open. An older server that kept running would write under the schema it knows: it would not fill a new column or table that newer code relies on.

- A connection in WAL mode holds a shared lock on the database file from its first read until it closes. `store::open` runs pending migrations on a connection in exclusive locking mode. Its first transaction takes an exclusive lock on the file and fails while any other connection is open, read-only ones and idle ones included. The migrations, and the schema statements before them, run only after the lock is held, so a refused attempt changes nothing.
- One Davy at a time tries, the one that holds a lock (`flock`) on `davy.sqlite.lock` beside the database. It holds that lock until it has migrated or given up. Two attempts at once each hold the shared lock the other waits for, so both fail. Without the lock file, eight processes of the same version that start at the same moment, such as `xargs -P8 davy issues`, failed each other for the whole 10 s: each attempt lasted about 130 ms, the pauses between them were shorter, and with eight openers out of step another attempt was always running. Eight threads that open an outdated store together, on an M2 Max while two full `cargo test` runs went on beside them (load average 7 to 15): without the lock file, 4 of 300 rounds failed and 2 of 150 more took over 4.5 s. With it, 350 rounds passed and the slowest took 0.53 s. `davy.sqlite.lock` is created with mode 0600 and never deleted, so every process locks the same file. It is not the database file, so closing it drops no lock SQLite holds.
- One attempt waits 100 ms for the lock on the database, not the 5 s busy timeout, which is enough for another Davy's read of `user_version`. A Davy that finds `davy.sqlite.lock` locked, or whose attempt fails, pauses for a random 10 to 110 ms, reads `user_version` again and tries again. When another one has migrated meanwhile, it opens the store without migrating. That read waits the busy timeout while another process migrates.
- `store::open` keeps trying for about 10 s. It checks the deadline between steps, so a read of `user_version` that starts just before can add up to the 5 s busy timeout while another process holds the database alone. Then it stops with an error that another process has the database open, and that an older Davy server must be stopped or restarted on this version. When the read of `user_version` still finds the database locked after 10 s, or another Davy still holds `davy.sqlite.lock`, the error says that another `davy` is probably migrating it.
- The first transaction is `BEGIN EXCLUSIVE`, not `BEGIN IMMEDIATE`. On a WAL file both take the exclusive lock when the WAL opens. On a fresh file, which is in rollback mode until the first migration switches it to WAL, `BEGIN EXCLUSIVE` takes the lock in one step. With two threads that each made one attempt on a fresh file at once, 100 rounds in a scratch `cargo test` on an M2 Max while other builds ran, at least one attempt failed in 6 rounds with `EXCLUSIVE` and in 43 with `IMMEDIATE`. The retries make both reach the same end.
- After the migrations the connection closes and the store opens again in normal locking mode, so the server and the CLI share the database as before.
- When no migration is pending, `store::open` takes no exclusive lock and does not open `davy.sqlite.lock`.
- The locks are POSIX locks, which belong to the process and the file. Closing any descriptor of `davy.sqlite` drops every lock the process holds on it, so Davy never opens the database file outside SQLite: no `File::open`, `fs::copy` or hash of it. Copy it with SQLite's backup, or stop the server first.
- `SCHEMA` in `store.rs` runs on every open, outside the lock, so it stays the schema of the first release. A new table or index is a migration.
- A connection that was opened but has not read yet holds no lock, so it does not stop a migration. Its first read waits for the migration as long as the busy timeout allows, then fails. The server opens its writer with `store::open`, which reads `user_version`, so a running server always holds a shared lock.

### Payloads of hot issues

SQLite appends new rows at the right edge of the `events` table and fills each page before it moves on. Until migration 19 the newest event of an issue kept its payload in its own row, marked with an empty zstd skippable frame, and the next event cleared it. The space cleared sat in a page that was already full and left behind, and SQLite reuses such space only when a row of that page grows again, which an event row never does. `auto_vacuum` returns only whole free pages. So an issue that keeps 150 payloads per 10,000 events took the disk of 10,000 payloads.

Three designs were measured with `examples/error_footprint.rs` on an M2 Max (Mac14,5, 32 GB, macOS 26), a release build, the Sentry Node 10 event in `e2e/fixtures/sentry-node-10/4.body` with a fresh event id and message per issue, and 10 issues of 10,000 events each, interleaved. A run ingests 14 days, then twice expires the oldest 7 and ingests 7 more, so the bytes per event are those of a store past its retention: the file's size over the events left. "Compacted" is the size after `VACUUM`. The shared dev machine had a load average of 15 to 25, so writer times differ by 10% or more between runs of one design.

| Design (`sim`, bare SQL) | Bytes per event, 1 event per transaction | Writer, µs per event | Bytes per event, 64 per transaction | Writer, µs per event |
|---|---|---|---|---|
| `current`: the newest payload in its `events` row, cleared | 2,168 | 162 | 570 | 19.4 |
| `table` (a): every kept payload in a `payloads` table keyed by event | 272 | 169 | 272 | 18.0 |
| `side` (b): the sample in `events`, the newest in `latest_payloads`, one row per issue | 270 | 97 | 270 | 16.0 |
| `autovacuum` (c): `current` with `auto_vacuum=INCREMENTAL` and `incremental_vacuum` after expiry | 2,171 | 169 | not run | not run |

Compacted, every design took 240 bytes per event, so (a) and (b) stay within 13% of the compacted size and `current` took 9 times it (derived from the table). (c) does not help, because the holes are inside pages that still hold rows. With 64 events per transaction `current` wastes less, because the writer compresses the newest payload of an issue once per transaction, so only one event in 64 per issue left a hole. Cold issues, 5,000 issues of 20 events at 64 per transaction, every payload kept: `current` 2,177 bytes per event, (a) 2,306, (b) 2,177. (a) costs a second row and an index entry for every kept payload, 130 bytes per event here (measured). (b) adds nothing per event and writes one row per issue per transaction.

(b) is what Davy does. The same harness through the real writer (`store`), before and after, 10 issues of 10,000 events:

| Run | Bytes per event | Writer, µs per event |
|---|---|---|
| before, 1 event per transaction | 2,169 | 365 |
| after, 1 event per transaction | 271 | 350 |
| before, 64 per transaction | 573 | 64.0 |
| after, 64 per transaction | 271 | 59.9 |

After the change the store compacts to 241 bytes per event. At 1 event per transaction the writer time is mostly the commit's fsync, and another run after the change took 182 to 197 µs. Cold issues, 5,000 issues of 26 events at 64 per transaction: 2,228 bytes per event before and after, 104 and 97 µs (measured).

Reads pay for the second table. `reads` times each query 50 times per round, in turns over the store written before and the one written after, and prints the median of 15 rounds: the newest stored payload of an issue (`issues::stored_payload`, used by the issue page, `davy event` and `issue_context`) went from 5.4 to 8.1 µs, and a page of 50 events with their `stored` flags from 25.1 to 28.3 µs. Over 5,000 issues of 26 events, where no issue has a row in `latest_payloads`, the payload went from 6.1 to 8.4 µs and the page from 32.9 to 35.6 µs. About 3 µs per call, on calls made once per page view or tool call. Part of it is the layout: the old query on the new store takes 6.8 to 7.1 µs, because the newest event in `events` no longer has a payload and the query walks back to the last sampled one. `prepare_cached` and a `UNION` of the two tables were slower.

The `sql` tool sees the same tables: `events.payload` holds the sample, `latest_payloads.payload` an issue's newest, and the tool's description and `NOTES` say how to read both.

Retention deletes an issue's `latest_payloads` row when its event expires. The work bound of a sweep counts that row's payload as the event's own, read by the row's key.

Migration 19 creates `latest_payloads` and moves into it each payload with the mark, found with one index lookup per issue on the issue's newest event, then clears it in `events`. `error_footprint upgrade` timed it on a store from `store` with 100,000 issues added by SQL, each with two events and a marked 2 KB payload on the newest. Over the 100,010 issues it took 3.3 to 4.0 s in two runs, and the file grew from 465 MB to 670 MB, because the payloads are written again and the space they leave in `events` is the same kind of hole as before. SQLite writes them to the write-ahead log first, so the upgrade needs free disk of about twice the size of the marked payloads, at most one per issue, 400 MB in that run, or it fails and rolls back. Space an existing store lost stays lost until retention empties those old pages, or until an offline `VACUUM`, which needs the server stopped and free disk the size of the store. Going back to an older Davy means restoring the backup taken before the upgrade. An older server that kept running would go on writing marked payloads into `events`, where they stay, and its retention would hit the new table's foreign key, which is why the migration waits for it to stop (see "Migrations").

### CLI

- `davy serve --data <dir> --listen <addr> --public-url <url>`
- `davy project create <slug> --data <dir> --public-url <url>` prints the DSN `http(s)://{public_key}@{host}/{project_id}`.
- `davy issues --data <dir> [--project <slug>] [--discarded] [--json]` lists issues, newest `last_seen` first, with id, status, count, title, culprit. The text view writes `(discarded)` after the status of a discarded issue, `--json` gives `discarded_at`, null when it is not discarded, and `--discarded` lists only discarded issues.
- `davy event <issue_id> --data <dir>` prints the latest stored payload as JSON.

### Done means

- Unit tests parse every fixture in `e2e/fixtures/` and assert the exact item kinds and the exception type and value.
- Grouping tests assert literal values: two events that differ only in a number or a UUID in the message land in one issue, and different exception types land in two.
- `e2e/run.sh` builds the binary, starts `davy serve` on a free port in a temp data dir under `.scratch/`, creates a project, runs a Node script against it with `@sentry/node` 7 and 10 (two captures of the same error and one different error per SDK), and asserts with `davy issues --json` that there are exactly the expected issues and counts. It stops the server at the end.

### Known follow-ups

- A single bad event in an envelope fails the whole request with 400. Relay drops only the bad item. Decide when the first real case shows up.

## Milestone 3: logs

Four wire formats come in. One `LogRecord` shape goes out to one store. Each source is an adapter: a pure function from bytes to records, unit-tested on captured fixtures, except Vercel's, which no drain has been captured for yet and which is tested on the examples of Vercel's reference. The HTTP layer does auth, size caps and decompression the same way for all of them.

### Wire protocol (what we accept)

| Source | Route | Auth | Body | Success |
|---|---|---|---|---|
| Logtail (`@logtail/node`, `@logtail/pino`, Vector, Fluent Bit) | `POST /` | `Authorization: Bearer <public_key>` | msgpack, JSON or NDJSON | `202`, empty body |
| OTLP/HTTP logs | `POST /v1/logs` | `Authorization: Bearer <public_key>` | protobuf (`application/x-protobuf`) or JSON (`application/json`) | `200`, empty `ExportLogsServiceResponse` in the request's encoding and content type |
| Sentry `log` item | the existing envelope route | the existing Sentry key rules | `{"items":[...]}` inside an envelope | the existing `200 {"id":...}` |
| Vercel Log Drain, custom endpoint | `POST /vercel/logs` | `Authorization: Bearer <public_key>` as a custom header, plus `x-vercel-signature` for a project with a drain secret | JSON or NDJSON | `200`, empty body |

- The bearer token is the project's `public_key`, so one DSN key also works as a Logtail source token and an OTLP header (`OTEL_EXPORTER_OTLP_HEADERS=Authorization=Bearer <key>`). The token alone picks the project.
- All four use the existing `Content-Encoding` handling and the 20 MiB / 64 MiB caps and the memory budget. The token is checked before decompression, and so is a Vercel drain signature, which covers the body as sent.
- Errors. `401` missing token, `403` unknown token, `413` too large, `429` with `Retry-After: 1` when the log queue is full. An undecodable Logtail body is `406` (what Better Stack answers). An undecodable OTLP body is `400` with a `google.rpc.Status` in the request's encoding. An OTLP content type other than protobuf or JSON is `415`. An undecodable Vercel body is `400`, since Vercel documents no status for it. A missing, malformed or wrong `x-vercel-signature` to a project with a drain secret is `403`, the status of Vercel's sample endpoint.
- Lenient on fields here too. A record that is not an object is skipped. A field of the wrong type falls back to a default. A batch never fails because of one record.

### Data shape

```rust
pub struct LogRecord {
    pub project_id: i64,
    pub timestamp: i64,          // unix nanoseconds; logs need sub-millisecond order inside a burst
    pub observed_at: i64,        // unix nanoseconds, when Davy received it
    pub source: LogSource,       // Logtail | Otlp | Sentry | Vercel
    pub level: LogLevel,         // Trace < Debug < Info < Warn < Error < Fatal, stored as 1..=6
    pub level_unknown: bool,     // no level, or one not understood; in flight only, never stored
    pub message: String,
    pub service: Option<String>,
    pub trace_id: Option<String>, // 32 lowercase hex, None when empty or all zeros
    pub span_id: Option<String>,  // 16 lowercase hex
    pub attributes: serde_json::Map<String, serde_json::Value>,
}
```

### Log levels

One level table for every source (`LogLevel::parse`). Logs default to `Info`, unlike error events.

| Input | Level |
|---|---|
| number (pino): ≤10, ≤20, ≤30, ≤40, ≤50, above | Trace, Debug, Info, Warn, Error, Fatal |
| a string that is a finite number, such as `"30"` or `"50.0"` | as the same number |
| OTLP `severity_number` 1-4, 5-8, 9-12, 13-16, 17-20, 21-24 | Trace, Debug, Info, Warn, Error, Fatal |
| string, case-insensitive: `trace`, `t`; `debug`, `d`; `info`, `information`, `notice`, `log`, `i`; `warn`, `warning`, `w`; `error`, `err`, `severe`, `fault`, `failure`, `exception`, `e`; `fatal`, `critical`, `crit`, `alert`, `emerg`, `emergency`, `panic`, `f` | in that order |
| OTLP `severity_number` 0 | parse `severity_text` |
| anything else, or no level | Info, marked not understood |

Before a string is looked up, whitespace and the characters `[ ] ( ) < > :` around it are trimmed and only the text after its last `.` is kept, so `[error]`, `error:`, ` WARN ` and `Level.ERROR` are read as their level. A level that was missing or not understood is stored as Info and counts as Info everywhere, but the record carries `level_unknown` while it is in flight: a drop rule of `--level` never matches such a line (see [Drop rules](#drop-rules)), as nobody can tell it is not an error. The flag is not stored.

One timestamp function for every source. A number below 1e11 is seconds (fractions allowed), below 1e14 milliseconds, below 1e17 microseconds, else nanoseconds. Fractional numbers are rounded to the microsecond, because an f64 holds about 16 significant digits and the nanosecond digits of a seconds value are noise. A string is RFC 3339, with a naive `YYYY-MM-DDTHH:MM:SS[.f]` or a bare date read as UTC. A msgpack timestamp extension (type -1, 4, 8 or 12 bytes) is decoded per the msgpack spec, which allows at most 999,999,999 nanoseconds. `@logtail/pino` sends `dt` as an RFC 3339 string (captured fixture), but other msgpack shippers may encode a date natively. Anything else, or a time older than its project's log retention or more than 24 h in the future, becomes `observed_at`, and the original value is kept in the attribute `davy.original_timestamp`.

### Adapters

**Logtail.** Content type `application/msgpack` or `application/x-msgpack`, `application/json`, `application/x-ndjson` or `application/ndjson`. Without a content type, a body whose first non-space byte is `[` or `{` is JSON or NDJSON, else msgpack. The body is an array of records, one record, or one record per line. An NDJSON line that is JSON but not a record is skipped like any bad record. A msgpack body holds exactly one value, so bytes after it are a `406`, and it nests at most 129 levels, as JSON does. In each record `dt` is the timestamp, `message` the message (a non-string is written as compact JSON), `level` the level. `service`, `trace_id` and `span_id` are lifted out when they are strings, the ids only when they are valid hex. Every other key stays in `attributes` as it came, nested objects included. Decode msgpack with `rmpv`.

**OTLP.** Use the `opentelemetry-proto` message types (prost) for protobuf. Check its serde JSON against a real exporter capture first. OTLP JSON writes trace and span ids as hex, not base64, 64-bit integers as strings or numbers, and enums as integers. If the crate gets any of that wrong, parse the JSON by hand. Mapping:

- resource attribute `service.name` becomes `service`. Other resource attributes go into `attributes` as `resource.<key>`. A non-empty scope name goes in as `otel.scope.name`, a non-empty `event_name` as `otel.event_name`.
- the timestamp is `time_unix_nano`, else `observed_time_unix_nano`, else `observed_at`.
- a string `body` is the message. Any other body is written as compact JSON.
- `AnyValue` converts to JSON as string, bool, integer, float, array, object (from a kvlist), and base64 string (from bytes).

**Sentry `log` item.** `ItemKind::Log` moves from `Drop` to a new `Disposition::StoreLogs`. The item header has `item_count` and `content_type: application/vnd.sentry.items.log+json` but no `length`, so the payload runs to the end of its line. The payload is `{"version":2,"items":[{"timestamp": <float seconds>, "trace_id", "span_id"?, "level", "body", "severity_number"?, "attributes": {"<key>": {"value": ..., "type": ...}}}]}`. Attributes are flattened to `<key>: value`. `service.name`, when present, becomes `service`. An envelope with only log items still answers `200 {"id": ...}`. A log item that does not parse is skipped and the rest of the envelope is stored, as with other unknown items.

**Vercel Log Drains.** The body of a custom drain endpoint (<https://vercel.com/docs/drains/reference/logs>): a JSON array of records, one record, or one record per line (NDJSON). The content type does not matter: a body that is not one JSON value is read as NDJSON, and a record that is not an object is skipped, as for Logtail. Mapping:

- `timestamp`, unix milliseconds, is the timestamp, through the one timestamp function.
- `level` (`info`, `warning`, `error`, `fatal`) through the level table, so `warning` is Warn.
- `message` is the message. Vercel cuts it at 256 KB and Davy at 64 KiB, as for every source.
- `projectName` becomes `service`, or `projectId` when there is no name. `projectId` stays an attribute too, since the Vercel API takes it.
- `traceId` and `spanId` become `trace_id` and `span_id` when they are valid hex. Older drains also send `trace.id` and `span.id` with the same values: they are read when `traceId` is missing and dropped when they hold the id already lifted.
- Every other key stays in `attributes` under Vercel's own name, nested `proxy` object included, so the drain reference is the attribute reference: `attributes.statusCode:500`, `attributes.source:lambda`, `attributes.proxy.clientIp`. `clientIp` is scrubbed like any IP ("Server-side data scrubbing").
- Vercel's `id` stays an attribute and is not used to drop duplicates. No log source is deduplicated, so a batch that Davy stored and Vercel sends again, after a lost answer or a timeout, is stored twice.

No drain has been captured yet. The adapter and the route follow Vercel's documented schema and signing and have not been checked against a live drain, and its tests use the two example records of Vercel's reference verbatim, as JSON and as NDJSON.

**Vercel drain signatures.** Vercel signs each drain request: `x-vercel-signature` is the hex HMAC-SHA1 of the body under the drain's signature verification secret. A project can keep the drain secret, and then a request to `/vercel/logs` for it needs both the key and a valid signature. That is all the secret proves: a log with source `vercel` in such a project came in a request signed with the secret, or in a replay of one, since the signature covers only the body and holds no time. It does not close the project to anyone else with the key. The key is in every DSN of the project, and it still writes logs through `/` and `/v1/logs` and events through the envelope routes, signed or not. So each drain should get a Davy project of its own, whose key lives only in the drain's custom header. Whether a project with a drain secret should refuse the other log routes is open.

- `davy project vercel <slug> --data <dir> --secret-stdin` stores the secret in `projects.vercel_secret` (migration 21, which gives an existing store the column empty, so no project needs a signature until one is set), `--no-secret` clears it, and the command alone prints `{"project":...,"signature_required":...}`. The secret comes in on standard input, never as an argument, so it stays out of `ps` and shell history. It is one line of at most 1,024 bytes without control characters, read without the spaces and line breaks around it, and an empty one is refused. No output, error or log holds it, and the SQL tool reads the column as NULL, as it does `channels.secret`.
- It is stored as Vercel shows it, not hashed, because checking an HMAC needs the key, as for the signing secret of an alert channel.
- The server reads it per request, after the key and before the body, so a change applies to the next request with no restart. A missing or malformed header (not 40 hex digits) is a `403` before the body is read. The HMAC is checked once the body is read and before it is decompressed or parsed, in constant time.
- The signature covers the body exactly as sent, as in Vercel's sample endpoint, which hashes the raw request body. Vercel does not document compressed deliveries. A body with a `Content-Encoding` is decompressed under the usual caps only after its bytes as sent match, and a body signed in its decompressed form is a `403`.
- A project without a secret takes drain requests with the key alone and ignores the header. Vercel tests the endpoint when a drain is created, and its docs do not say whether that test is signed. The secret can be set in Davy before the drain is created, since Vercel shows it in the form; if the creation test then gets `403`, it was unsigned, and clearing the secret, creating the drain and setting the secret again is the way through.

### Storage (Tantivy, one index per UTC day)

- `data/logs/YYYY-MM-DD/` is one Tantivy index for the UTC day of `timestamp`. Docstore compressed with zstd, as in the benchmark.
- Fields: `project` u64 indexed and fast. `ts` i64 indexed, fast and stored. `observed` i64 stored. `level` u64 indexed, fast and stored, so `--level warn` is a range query. `source` u64 stored. `service`, `trace_id` and `span_id` raw strings, indexed and stored. `message` text, stored. `attributes` a JSON field, indexed with the default tokenizer and stored, so `attributes.user_id:42` works. `values` text, not stored, holds every string and number in `attributes` at any depth. A bare word in a JSON default field matches only the root path, which no attribute has, so without `values` a request ID in `attributes.req.id` was found only by its path. Day indexes created before `values` existed keep their schema, and the writer and search look the field up per day. `pattern` u64 indexed and stored, the id of the record's pattern ("Log patterns"); a day created before it has none.
- The JSON field stays. On 1M synthetic rows it took 0.48× the raw NDJSON size, under the 1.0× bar set before measuring, so the flat text variant was not measured. Numbers in `docs/storage-benchmark.md`, section "Logs ingest".
- One log writer thread owns the open `IndexWriter`s (at most 3 days open, least recently written one closed first, a day idle for 10 minutes closed). It reads batches of `LogRecord` from a queue bounded at 50k rows and 128 MiB of heap, counted in records rather than batches because one batch holds one record or thousands and memory follows the records. A record's heap is priced by `heap::of_record`: the record, its text and its attribute tree with the allocator's slack, so 50k records of 1 MiB each, or of a thousand empty attributes each, cannot fill memory. A batch holds its share from the moment it is queued until the writer has indexed it, not only while it waits. A full queue is the `429`. A batch larger than the whole queue is `413`, because it would never fit. A bound of 256 batches let 1M rows in batches of 1000 push RSS to 828 MB (measured). Each writer uses 1 indexing thread and a 64 MB heap (1 thread was faster in the benchmark).
- The writer commits every second when anything changed, and at once when 100k documents are uncommitted. It commits everything on shutdown.
- A failed `add_document` or commit drops that day's `IndexWriter`, and the next write opens a fresh one. Tantivy does not restart its indexing worker after a failed commit, so the next 10k documents would block the writer thread forever and ingest would answer `429` from then on (reproduced in `store_recovers_when_a_day_index_breaks`). The cost is that the uncommitted documents of that day, at most about one second, are lost and logged.
- The HTTP handler answers once the batch is queued, not once it is committed. Error events wait for their commit because they are few and each matters. Logs come in bulk and the shippers retry on failure only. The cost is that a crash loses at most about one second of accepted logs, and a log shows up in search within about one second.
- Retention: `davy serve --log-retention-days <n>`, default 30, from 1 to 36,500 (100 years, past that the nanosecond cutoff overflows), and a project can keep its logs for a number of days of its own. At start and every minute the writer renames day directories older than every retention into `data/logs/.trash/` and removes them. It touches only real directories whose name is the canonical `YYYY-MM-DD` of a date, never a symlink, and empties `.trash` only when it is a real directory, and only of real directories named `YYYY-MM-DD-<digits>`, the names it gives them. Anything else there stays and is reported on stderr. Search and the writer take the same days: a symlink or a file in a day's place, or a directory with another name for a date, such as `+002026-01-05` or a year of six digits, is neither searched nor written. A log of such a day is dropped with an error, like a log of any day that cannot be opened, while the batch's logs of other days are written. Retention reports each one on stderr once, and again if it comes back after it was moved away, with the path quoted and escaped so a name cannot forge a line of the log, and `davy doctor` warns about it. Following a link would let a deletion leave `data/logs`, and a second name for one day would split its logs. "Retention per project" says how a project with a shorter or longer retention is cut out of a day.
- Closed days older than `--hot-days`, 7 by default, turn into compressed cold files, searched the same: see "Cold tier".

### Search

```rust
pub struct LogQuery { pub project: Option<i64>, pub since: i64, pub until: i64,
                      pub min_level: Option<LogLevel>, pub text: Option<String>,
                      pub pattern: Option<u64>, pub limit: usize, pub skip: usize }
pub fn search(data: &Path, q: &LogQuery) -> anyhow::Result<Vec<LogHit>>   // newest first
```

- Opens only the day indexes that overlap `since..until`, newest first, sorts by the `ts` fast field and stops once it has `limit` hits.
- Keeps each day it opened open for the next search (`logs/days.rs`). Opening a day costs about 1 ms, so a rare needle over 90 days took 86 to 101 ms before and 2.3 to 6.0 ms after (measured, `docs/storage-benchmark.md`). A day is reopened when the size, mtime or inode of its `meta.json` changed, because every commit replaces that file, so only today's day reopens. At most 400 days stay open, least recently searched closed first, so the memory maps stay well under Linux's 65,530 per process. Each segment caches 8 decompressed doc store blocks instead of Tantivy's 100. Retention drops a day from the cache before it moves the directory to the trash.
- `text` goes through Tantivy's `QueryParser` with `message` and `values` as default fields, or `message` and `attributes` in a day without `values`. So `timeout`, `r-3fa9c2d1`, `"connection reset"`, boolean queries such as `timeout AND db` and `attributes.user_id:42` all work. A parse error is shown to the user, not a panic.
- The CLI reads the indexes from its own process while the server writes. The writer's segment cleanup can delete a file just as the CLI opens it. Retry the search once on a missing-file error. The MCP server in milestone 4 runs inside the server process and avoids this.

### CLI

`davy logs --data <dir> [--project <slug>] [--since 1h|30m|2d|<RFC 3339>] [--until ...] [--level warn] [--pattern <id>] [--limit 100] [--json] [query]`

`davy logs --patterns` prints the top patterns of the window instead ("Log patterns").

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
- `e2e/run.sh` also sends logs through `@logtail/pino`, Sentry `logger.*` and both OTLP exporters, polls `davy logs` until all 10 records are searchable, and asserts with `davy logs --json` the exact message, level, service and one attribute per source, plus a trace id where the client sets one. A wrong bearer token gets 403. It then posts a Vercel drain batch with `curl`: with the project key alone before the project has a drain secret, then refused with 403 unsigned and wrongly signed, then signed with the secret, and asserts both records, the trace id, the scrubbed client address and a search by attribute.
- A load script sends 1M synthetic Logtail records in msgpack batches of 1000. Report rows per second and disk over raw size, with what limits them. It is a number to explain, not a gate.
- `cargo test` passes and `cargo clippy --all-targets -- -D warnings` is clean.

## Milestone 4: MCP and CLI for agents

A coding agent should get from "there is an error" to a fix without a human copying stack traces. It needs to read everything Davy stores, see one issue with everything around it, and say "fixed" in a way that reopens the issue if the error comes back after the fix ships.

### Transport and auth

- MCP over Streamable HTTP at `/mcp` inside `davy serve`, stateless, JSON responses (rmcp `legacy_session_mode = false`, `json_response = true`). In-process, so `mark_fixed` goes through the writer thread and log search never races segment cleanup. One port for ingest and agents. No stdio server: an agent on the same machine connects to `127.0.0.1`.
- `DAVY_TOKEN=<secret> davy serve`, at least 32 characters. There is no command line flag, because arguments show up in `ps` and shell history. `/mcp` takes `Authorization: Bearer <secret>`, the admin token or an agent token of its own, whose level decides which tools it reaches (see [Agent tokens](#agent-tokens)). Without a configured admin token `/mcp` takes agent tokens alone, and answers `401` to anything else. A project's ingest key is never accepted on `/mcp`, because it ships inside browser bundles.
- The token is compared as a blake3 hash, so the comparison takes the same time for every wrong token.
- Allowed `Host` values are loopback and the host of `--public-url`, against DNS rebinding. A request with an `Origin` must come from loopback or the public URL, so another site's page gets `403` even before the token is checked.

### Tools

| Tool | Input | Output |
|---|---|---|
| `schema` | none | The `CREATE TABLE` statements of the SQLite tables, what the columns mean, and the log query syntax. |
| `sql` | `query` | `{columns, rows, truncated}`. One read-only statement. |
| `search_logs` | `query?`, `project?`, `since?`, `until?`, `level?`, `pattern?`, `limit?` | Hits as `davy logs --json` prints them, oldest first. When more match than `limit`, the newest win. |
| `count_logs` | `query?`, `project?`, `since?`, `until?`, `level?`, `pattern?` | `{total, step, buckets}` as `davy logs --count --json` prints it. At most 200 buckets of a round width, and only those with logs, as `[start, count]`. |
| `log_patterns` | `project?`, `since?`, `until?`, `level?`, `limit?` | The patterns with the most bytes, as "Log patterns" says: id, template, example, records, bytes, share, levels and services. `since` defaults to `24h`, `limit` to 20, at most 100. |
| `connect_project` | `slug`, `sdk?` | Existing project DSN, SDK instructions and test command. New projects require local CLI access. |
| `project_status` | `slug` | Event count, last event time (ms), issue count, last log time (ns), `retention`: the days of logs and errors the project keeps, and `davy`: Davy's own warnings, errors and crashes of the last 24 hours (see "Self-monitoring"). |
| `set_retention` | `project`, `logs_days?`, `errors_days?` | What the project keeps now, as `retention` in `project_status`. A setting is 1 to 36,500 days or `"default"`, and may only keep data longer, unless a person unlocked destructive changes for the token. |
| `usage` | `project?`, `since?`, `what_if?`, `view?`, `top?` | What each project sent and when the disk fills: `disk`, `davy`, `growth_per_day` of all logs together, `longest_log_days` and `shortest_log_days`, `forecast`, `projects` with records and bytes accepted per signal over the window and per day and `logs_stored`, records and raw bytes of the logs stored by index day, and `logs_filtered`, those live drop rules dropped by day of receipt, and `limits`, the project's daily quotas against today's use, its spike rate and today's refusals, and `no_forecast` or `disk_error` when there is none. Sizes as `4.1 GB`. See "Usage and disk forecast". `view` `savings` answers with where logs and errors could take less instead, at most `top` entries of each kind; see "Savings". |
| `drop_rules` | `project?` | `{rules, note}`: the drop rules in force, in the order the log writer tries them, as `davy drop list` shows them with sizes as strings, a shadow rule with `make_live`, the command a person runs to make it live. |
| `drop_rule_shadow` | `project`, exactly one of `pattern`, `level`, `message`, then `service?`, `keep?` | `{rule, note}`: the shadow rule added, or the equal one the project has. No field makes it live. Refused on Davy's own project and for a level rule of warn or above. |
| `drop_rule_remove` | `project`, `rule_id` | `{removed}`. Only a shadow rule an agent added; its counts go with it. |
| `drop_rule_live` | `project`, `rule_id` | `{rule, note}`: the rule, live now and a person's. Only for a token a person unlocked for destructive changes, see [Agent tokens](#agent-tokens). |
| `ledger` | `actor?`, `before?`, `limit?` | `{rows, next_before}`: what agents and people changed through Davy, newest first, see [Ledger](#ledger). |
| `release_health` | `project?`, `since?` | Per release and environment, newest first, at most 50: `sessions`, `errored`, `crashed`, `abnormal`, `crash_free_sessions`, `users`, `crashed_users`, `crash_free_users`, `first_hour`, `last_hour`. `since` defaults to `7d`. |
| `issue_context` | `issue_id` | The issue, its latest event made compact, its recent events and releases, and the logs around the latest event. |
| `mark_fixed` | `issue_id`, `release?` | A sentence that says which release the fix is expected in. |
| `submit_feedback` | `tried`, `happened`, `fix?` | A sentence that says the feedback is stored. |

**`sql`** opens its own read-only connection per call (`SQLITE_OPEN_READ_ONLY`, `PRAGMA query_only`). Like every read path, it neither creates nor upgrades the database: a data directory without `davy.sqlite`, or with one whose `user_version` is older than this build's migrations, is an error that says to run `davy serve`, and one from a newer Davy is refused with the same error a writer gives (`store::refuse_newer`). A read-only open of a WAL database creates its `-wal` and `-shm` files when they are missing, as the reader's user, and leaves them. When both are missing, so no connection has the database open, and the reader may not write the directory, as on a read-only mount, SQLite cannot create `-shm` and the read fails with an error that says so and asks to copy the data directory or read as a user who may write it. Davy never opens the database with `immutable=1`, which would read it there: nothing stops a server, perhaps as another user or through another mount, from opening it a moment later, and an immutable reader then mixes old and new pages without an error. The path is opened as a path, not a URI, so any path SQLite opens works, `//` at its start included. The connection prepares exactly one statement, and runs it under an authorizer that allows only reads and function calls. `stmt.readonly()` is not enough, because SQLite counts `ATTACH` and `PRAGMA query_only=0` as read-only. At most 500 rows. A text cell over 4000 characters is cut and ends in `…[cut]`. A string or blob over 16 MiB is an error, so one query cannot take a gigabyte. A blob cell is `{"blob_bytes": n}`. The function `payload_json(payload)` decompresses an event payload to JSON text, so `json_extract(payload_json(payload), '$.request.url')` works. A query is interrupted after 5 s. A bad query is a tool error with SQLite's message, not a protocol error.

**`issue_context`** returns:

- `issue`: id, project slug, title, culprit, level, status, `event_count`, `first_seen`, `last_seen`, `resolved_in_release`, `regressed_at`, `discarded_at` (null when it is not discarded; `last_seen` stops at the mark while the error goes on). Times are RFC 3339.
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
- HTTP contract tests: `/mcp` is 401 without the token or with a wrong one and 403 for a `Host` or `Origin` it does not serve; with the right one `tools/list` and `tools/call` of `sql` and `mark_fixed` answer. Without a configured admin token it is 401 to every bearer token but a live agent token.
- `e2e/run.sh` connects with the official MCP TypeScript client (`@modelcontextprotocol/sdk`) over Streamable HTTP with the admin token, lists the seventeen tools it reaches, calls `issue_context` on an issue the Sentry SDKs created and asserts the exception type and an in-app frame, finds a log with `search_logs` and counts it with `count_logs`, finds its pattern with `log_patterns` and its lines by that pattern with `search_logs`, calls `mark_fixed` and reads the status back with `sql` and the row it left with `ledger`, lengthens a project's retention with `set_retention` and sees a shorter one refused, and reads the counts of the e2e projects with `usage`.
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

- `type` is one of `issue.new`, `issue.regressed`, `issue.digest`, `monitor.down`, `monitor.up`, `quota.warn`, `quota.reached`, `feedback`, `test`. A quota notice has a `quota` object ([Quota alerts](#quota-alerts)). A `feedback` notice goes only to channels added with `--feedback`, has `project: null` and a `feedback` object with `id`, `client`, `tried`, `happened` and `fix`. Its Slack line is `Feedback from <client>: <happened>`.

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
- `docs/connect/` has one page per client: Node, the browser with source maps through `sentry-cli`, Python, Go, Logtail and pino, OpenTelemetry, and Vercel Log Drains. Each uses the official SDK or integration and changes only the DSN or token.
- `llms.txt` at the root of the repository lists these pages with one line each.
- Every command in a fenced `sh` block of these pages runs in e2e (Done means), so a page cannot fall behind the binary.

### Commands for agents

- `davy setup --data DIR --public-url URL [--listen ADDR] [--mail-from ADDR] [--service systemd|launchd|none]` prepares a machine. It creates the data directory readable only by its owner, writes a random admin token to `DIR/token` with mode 0600 when none exists, records its flags in `DIR/setup.json` for `serve`, `doctor` and `mcp-config`, writes a launchd job into `DIR` when asked and prints JSON: the paths it wrote and the next commands. Running it twice changes nothing and prints the same JSON.
- Setup runs as the user the service runs as and never needs root. It refuses a systemd service for root or for a user that does not exist, because the server parses untrusted input. It refuses a directory that holds files and none of Davy's or that belongs to another user, because it makes the directory private, and it refuses a link in place of its own files. The commands that need root (installing the unit, `systemctl`) are printed, not run, so the person sees each one.
- The systemd unit is never a file in the data directory. `davy systemd-unit --data DIR --user USER` prints it, run by the operator and piped to `sudo tee`. It holds the binary, `serve --data DIR` and the user, all checked, and nothing read from `DIR`: a service user that an attacker took over could otherwise rewrite the unit that root installs and run code as root. Paths with a character systemd reads as syntax are refused, so no word needs escaping.
- The systemd unit has `Type=notify`. The server sends `READY=1` to `NOTIFY_SOCKET` once it listens, so `systemctl restart davy` returns when requests are answered and the `davy doctor` that follows finds the server up. Found by `e2e/install.sh`, where doctor ran before the server listened.
- The systemd unit runs the server with write access to the data directory only (`ProtectSystem=strict`, `ReadWritePaths`), no capabilities, no new privileges and a private `/tmp`. `davy serve` reads the admin token from `DIR/token` and the SMTP URL, which holds a password, from `DIR/smtp-url` when `DAVY_TOKEN(_FILE)` and `DAVY_SMTP_URL(_FILE)` are unset, and the flags it was not given from `DIR/setup.json`.
- `davy serve` also reads the admin token from the file named by `DAVY_TOKEN_FILE`, so a service file holds a path, not the token.
- `davy doctor --data DIR [--json] [--log-retention-days N]` checks a running install. Each check has a status (`ok`, `warn`, `fail`) and, unless it is `ok`, the command or setting that fixes it. It checks that the data directory belongs to the user doctor runs as and that others cannot read it, the token's presence and length, that the server answers at the public URL, that its `/mcp` refuses a request without the token and answers an MCP `initialize` with the token in `DIR/token` by a JSON-RPC reply of at most 64 KiB, that the public URL is `https` unless it is a loopback address, that the SMTP server accepts a connection when one is set, that each entry of `DIR/logs` named like a day is a real directory with the name `YYYY-MM-DD`, which projects are at or past a daily quota today ([Quota alerts](#quota-alerts)), and free disk space. It sends nothing to the internet beyond the public URL and the SMTP server, and it sends the token only over `https` or to a loopback address, without a proxy. The exit code is 1 when a check fails.
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

The CLI and MCP serve agents and whoever lives in a terminal. A team also needs a place to see what broke, read a stack trace, search logs and check a monitor without a shell on the server. This milestone gives one team that place, inside the same binary. Roles and SSO stay in the closed cloud (see "License and open core"). Agent tokens and the ledger of what agents did came later, see [Agent tokens](#agent-tokens).

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
- `Recorded { job, entry }`: one of these with the ledger row that records who made it, written in the same transaction: a person's change from the web UI, and an agent's change that deletes data, see [Ledger](#ledger).
- `SetRetention(change, by)` for how long a project keeps its logs and errors, from `POST /ui/api/retention` (`By::Person`) and the MCP tool `set_retention` (`By::Agent`, or `By::Unlocked` for a token a person unlocked). An unknown project refuses the change with `unknown project "slug"`, and an agent's change that keeps either kind shorter than now with the reason. The writer decides, so two changes at once cannot slip a shorter one past the check.

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
| `GET /ui/api/usage?view&project&since&top` | what `davy usage --json` prints, see [The usage page](#the-usage-page) |
| `GET /ui/api/usage/days?project&since` | `UsageDays`, each project's counts per UTC day, see [The usage page](#the-usage-page) |

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

The look was decided on a prototype of the issue page and the issues list, built on fixtures from the e2e SDKs. Of three directions the owner chose the one called Instrument. It should feel like calm, exact test equipment for someone who triages hundreds of issues. The prototype is in [`docs/ui-prototype/`](ui-prototype/index.html) and opens from disk with no server. Its issues 1 to 4, 9, 10 and 14 come from the e2e fixtures, the rest and every hourly count are made up. The layout, the density and the heat strip of Instrument stay. Its colors, type and mark later moved to those of the website, so the app and the site read as one product. The app follows these rules:

- It is a tool people read for hours. Information density beats decoration, body text is a plain sans, and a monospace face is for code, stack traces and log lines only.
- Light and dark follow the system setting, each with its own palette. There is no theme toggle.
- Icons are a small set drawn for Davy, not a generic icon package.
- Every chart has a table form, identity is never color alone, and each chart palette passes a color-vision check in both themes.
- Motion is short and serves a change of state: a folded frame opening, a row leaving a list after resolve. CSS transitions do it, with no motion library. Nothing is hidden behind an entrance animation, and `prefers-reduced-motion` turns motion off.
- WCAG 2.2 AA. Every action works from the keyboard: `/` searches, `j` and `k` move through a list, `x` selects, `r`, `i` and `o` resolve, ignore and reopen, `Enter` opens, `Esc` closes.

**Layout.** A list view has three regions. A left rail of about 190 px holds the projects, the status and environment filters, and the sort, which the browser remembers. Every sort is newest or largest first, and the rail shows no counts per project, because the list API answers pages and no totals. The middle is the list in rows of 30 px, with the columns that **Views** names. A right inspector of about 310 px shows the row under the cursor without leaving the list: the title, the culprit, the nearest in-app frame with its crash line and source-map origin, counts, first and last seen, the releases and the 24-hour strip. `Enter` opens the issue page. It keeps the rail and gives the event the rest of the width, with tags and contexts as a two-column grid of keys and values, never chips. A bar at the bottom names the keys of the view. A narrow window loses the inspector first, then folds the rail behind a button. On a phone the views scroll inside the bar, so the page never scrolls sideways.

**The heat strip.** It is the one signature of the look. The last 24 hours of an issue are 24 small cells in one hue, light to dark, on one scale for every row. In a column of rows the eye finds a spike without reading a number. Hover or the arrow keys read out the hour and the count, and the same numbers sit in a visually hidden table. The issue page shows the strip large, with an hour axis and the count in each cell. Cells with few events stay faint on purpose, at the 2:1 against the surface that the ordinal check asks, and their counts are in the readout and the table.

**Color.** The app wears the same night as the website: warm dark surfaces, cream ink and brass. Dark is that night; light is daylight paper with deep brass ink, a tint of the same brass rather than a cream sheet. Brass is the one accent, for focus, the cursor row, links and the selected state. Regions part by tone, the rail and the panels a step off the list surface, not by a border on every box. The bar ends on an engraved brass rule, brightest under the mark. Shadows only lift tooltips and popovers. Contrast measured against the list surface, and the lowest on the cursor row:

| Token | Light | Dark | Contrast (light, dark) | On the cursor row |
|---|---|---|---|---|
| list surface | `#fdfcfa` | `#13110e` | | |
| rail | `#f5f2ec` | `#0d0c0a` | | |
| panel | `#f7f4ee` | `#1c1813` | | |
| ink | `#1e1913` | `#f4f2ee` | 17.0, 16.9 | 13.8, 13.3 |
| muted ink | `#595145` | `#aea89f` | 7.6, 8.0 | 6.2, 6.3 |
| faint ink | `#665e52` | `#9a948a` | 6.2, 6.3 | 5.1, 4.9 |
| accent, brass | `#6e5b2b` | `#e2c287` | 6.4, 11.0 | 5.2, 8.7 |
| focus ring | `#6e5b2b` | `#ecd09a` | 6.4, 12.6 | |
| cursor row | `#f2e4c4` | `#2f2619` | | |
| alarm (fatal, error) | `#ad1f3a` | `#ef6f7a` | 6.8, 6.5 | 5.5, 5.1 |
| warning | `#a04100` | `#fb8f4c` | 6.3, 8.2 | 5.1, 6.5 |
| ok | `#2a6e40` | `#86c79c` | 6.0, 9.6 | 4.9, 7.6 |
| heat, empty to peak | `#efeae0` to `#4a2c0e` | `#221e17` to `#f8dc9f` | | |

Every text color keeps at least 4.5:1 on the cursor row and on a button too. Fatal and error share the alarm color, and info uses the muted ink. The status colors keep away from brass: warning is an orange and alarm a rose red, at an OKLab distance from the accent of 10.2 and 16.6 in light and 11.9 and 19.5 in dark (measured, ×100). Each level still has its own drawn mark and its word next to it. The primary button is a brushed brass key with dark ink, `#1f1408`, the same in both themes, at 7.0:1 on its darkest stop. The heat strip is one brass hue, light to dark, and passes the ordinal check of the dataviz validator in both themes: monotone lightness, a visible gap between steps, and its first step at 2:1 or more against the list surface. The eight series hues of the usage chart did not change and pass the categorical check against both new surfaces.

**Type.** Three faces the binary serves itself, never from a CDN, all under the SIL Open Font License and subset to Latin: Mona Sans at 13 px for text, Hubot Sans at a width of 108 and weight 600 for page titles and the name, and Commit Mono for code, frames, ids and times. Tabular figures for every count. Weight and size make the hierarchy below the titles. The subsets carry their own family names, as the license asks of a modified font, and `ui/src/fonts/README.md` says how they were made. Together they weigh 92 KB, and `scripts/ui-pack.mjs` fails a build whose fonts pass 100 KB.

**Marks.** The Davy mark is the brass safety lamp with its flame lit that heads the README, drawn on a 16 grid, in the same colors per theme. Login, welcome, setup and the page for a wrong address hang it in a lit window drawn as a brass engraving, its sill on an engraved line. The working pages carry only the small lamp, in the bar and above an empty list, and nothing on them moves. The tab icon is the same lamp in fixed colors. Icons for levels, statuses, folding, copying and the source-map origin are drawn for Davy.

### Front-end dependencies

npm is a supply chain of its own, and Davy is a security product. The rules:

- pnpm with a frozen lockfile. Dependency install scripts do not run (pnpm's default), and a version newer than 7 days cannot be added (`minimumReleaseAge: 10080`, in minutes). That setting checks a version when it enters the lockfile, not again later.
- A short list of runtime dependencies, each named here with the reason: `react` and `react-dom`; `@tanstack/react-router` for typed search parameters; `@tanstack/react-query` for cached API reads; `@radix-ui/react-popover` for the one popover, with focus and dismissal done right. No chart library: the log histogram is at most 200 SVG bars drawn in about 200 lines, where `uplot` would add about 50 KB. Uptime decides again when it arrives. No CSS framework and no motion library: one plain stylesheet holds the tokens of "What it looks like", and CSS transitions are enough for the motion it allows. A new runtime dependency needs a line in this list.
- `scripts/check`, and so CI, fails on any known vulnerability in `ui/pnpm-lock.yaml` (osv-scanner), and on a license outside MIT, ISC, BSD, 0BSD, Apache-2.0 and Unlicense in a package that ships in the bundle (`scripts/ui-licenses.mjs`). Unlicense is there for `isbot`, which the router brings. The release SBOM lists the npm packages next to the crates.
- At most 200 KB of JavaScript compressed for the first load, checked by `scripts/ui-pack.mjs` on every build. It writes a Brotli copy next to each text file, and Davy sends that copy with `Content-Encoding: br` and `Vary: Accept-Encoding` when the browser accepts `br`. Assets carry content hashes and are served with `Cache-Control: immutable`. `index.html` is never cached. The fonts count apart, at most 100 KB together, and the binary grows by at most 2 MB.

### Order of work

1. Accounts and sessions: the tables, `davy user`, the writer jobs and their second queue, login, logout, welcome, `Accounts` and the route layer, the security headers and the `/ui/` serving with the no-UI page. Contract tests first.
2. The design prototype of the issue page and the list, then the direction written here.
3. The app shell, then Issues and Issue.
4. Logs with the histogram and live.
5. Uptime, Alerts, Projects, Setup and Account.
6. An internal pentest of the UI routes, recorded in `docs/security/`, before the release that ships the UI.

### Not in this milestone

Creating monitors, channels, projects and users from the UI, which needs roles and a public-only `Reach` for URLs typed in a browser. Dashboards and saved queries, alert rules beyond channels, traces, more than one team, roles, SSO, translations, and a mobile layout beyond a readable single column.

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

`day` is midnight UTC in unix milliseconds, so one row grows by addition and a busy app adds a few rows a day. A reason or category must be at most 64 characters of `a-z`, `0-9` and `_`, which every name the SDKs send is. Rows are kept 90 days and the sweep deletes older ones. Reasons that start with `davy_` are Davy's own: it writes them for items it refused itself, such as `davy_rate` for a check-in past its rate limit or `davy_daily_cap` for a session past the daily cap. An SDK's entry with such a reason is skipped, so no one who holds a DSN can add to Davy's own counts.

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
- A change goes through the writer queue as `Job::SetRetention`, like every other write of the server. The next sweep applies it: within 10 s for errors, within a minute for logs. The writer holds `retention::Lock` while it writes a batch that holds a change, until the batch is committed, and the log writer holds the same lock while it reads the settings and deletes from a day, so a change the server reports as saved is never followed by a deletion under the setting it replaced. The errors read the setting inside the delete statement on the same writer, so they need no lock. The other way round, a change made while the log writer deletes from a day waits for that deletion's commit, up to seconds on a day of a million logs, and error ingest and logins wait in the writer's queue behind it. It happens only when a setting changes during a deletion, and moving the cut off the log ingest thread would shorten it.
- The CLI writes to the same database from a process of its own, so the log writer also takes an advisory lock (`flock`) on the data directory itself while it holds `retention::Lock`, and `davy project retention` takes the same lock while it saves a change. A CLI change therefore waits for a day being deleted from, at most about one day's delete and its commit, and the next read sees it. The log writer takes the server's lock first and the directory second, and the writer and the CLI each take only one, so none waits on another in a circle. The lock goes when the directory is closed, so a crashed CLI or server holds nothing. It is the directory and not a lock file in it because the server owns its data directory and can always open it: a lock file that a CLI run as root created under the umask of 077 would be one the server may not open, so it could never delete logs again, and a file at that path could be a symlink or a FIFO. A log writer that cannot open or lock the directory stops the pass before it deletes, as when it cannot read the settings. It waits at most 2 s for the lock, because it writes logs on the same thread and a CLI stopped while it held the lock must not stop them; the pass is tried again with the usual backoff. So a holder that never lets go stops log deletion for as long as it holds the lock: a `davy project retention` stopped with Ctrl-Z, or any local user who may open the data directory and calls `flock` on it. Davy creates the data directory readable by its owner only, which keeps other users out; a directory with group or other bits, such as a `0755` bind mount, lets them in. The CLI waits at most 5 minutes. Either names the data directory and a likely holder when it gives up. A `retention.lock` an older build left behind is unused and can be deleted.

### Errors

The SQLite writer runs `Job::Sweep` every 10 s, and the errors take their turn in it next to the monitors, with a budget of their own of 100 ms (`retention::errors::BUDGET`). Each turn deletes one batch of expired events, then at most 200 expired user feedback rows, then unlinks at most 50 user feedback rows from issues whose last event expired, until none is left or the budget is spent. A batch of events stops at 200 events or at about 10 ms of work, whichever comes first (`WORK`). An event costs one unit, about 6 µs, and one more per 4 KiB of payload, user id, release and environment, which SQLite frees page by page. The last three are capped at ingest, but a row stored before a cap keeps its size, and `octet_length` reads that size without the value. The first event of each issue in the batch costs 50 more, about 0.3 ms, for the statements that keep the issue's counts and tags. Both costs come from the measurements below. A batch takes at least one event, so a sweep ends at most about 10 ms past its budget, plus the cost of one event with a large payload, however the events spread over issues. A batch of 200 events in 200 issues used to take about 60 ms. SQLite rewrites a whole row to unlink it, and 50 rows with every field at its limit are about 1.3 MB, about 320 pages, so an unlink batch is estimated at about a fifth of a batch of events. That estimate is not measured.

- An event expires by `received_at`, the time Davy stored it, so a client clock cannot keep an event or expire it early. The new index `events(project_id, received_at)` makes each project's expired events one range. User feedback expires by its own `received_at`.
- Deleting an event takes it out of its issue's `event_count` and `issue_hourly`, and sets `first_seen` and `last_seen` to the oldest and newest timestamp of the events left, as ingest keeps them: the smallest and largest timestamp, whatever order the events arrived in. A client clock can be ahead, so the event that expires first can hold the newest timestamp. Migration 16 adds the index `events(issue_id, timestamp)`, so each is one index lookup. It runs in the one transaction of the upgrade, after the check that no newer Davy migrated the store. Building it reads every event once, as long as the index on `events` of migration 14 did, and needs as much free disk.
- `issue_tags` gained `last_at`, when an event with that environment or release last arrived, and an index by issue. The migration backfills it from the events. A tag goes when its `last_at` is no later than the newest deleted event that carried it and no event left that arrived in that same millisecond carries it, so a filter by environment or release finds only issues that still have such an event. A project's events are deleted in the order they arrived, so an event left that carries the tag arrived after the newest one deleted, which makes `last_at` later, or in the same millisecond, as the events of one request do, which the index by project and arrival finds.
- **An issue whose last event expired is deleted**, with its hourly counts and tags. Its user feedback stays, unlinked, until it expires too. An issue can hold 1,000 feedback rows a day, so they are unlinked in batches of 50 in the sweep's turns, and the issue keeps an `event_count` of 0 until the last is unlinked or expires. Expired feedback is deleted with `RETURNING issue_id`, so an issue whose feedback expires in the same sweep as its last event is deleted too. Migration 17 adds the partial index `issues(id) WHERE event_count=0`, so finding those issues reads no other issue. If the error returns first, ingest starts the issue again as it would start a new one: unresolved, with the new event's title and seen times and a new-issue alert. It keeps its id and the feedback still linked. Keeping an empty issue would show a count of 0 and a first seen time with nothing behind it, and agents read counts as facts. When the error returns it is a new issue: unresolved, with a new-issue alert, because the resolved or ignored state went with the old one.
- **An issue id is never used twice.** SQLite gives a new row the highest id plus one, so deleting the newest issue would hand its id to the next one, and a link in an old alert or an agent's notes would lead to an unrelated issue. The new table `id_floor(name, id)` keeps the highest issue id ever deleted, and a new issue takes one more than the larger of that and the highest id in `issues`. AUTOINCREMENT would do the same but needs the table rebuilt.
- A row of another table that still points at an issue keeps it with an `event_count` of 0, rather than fail the writer's batch, which holds ingest too.
- `releases` stays: it says when a release was first seen, which decides whether a resolved issue reopens. Release health counts sessions, not events, and keeps its own 90 days.
- A lower `event_count` moves which later events the payload sample keeps, so the sample still keeps about one in 100. It does not clear a payload that is in the sample: a new event replaces only the row in `latest_payloads`. An issue's row there goes when its event expires, which can happen before older events go when the server clock stepped back.
- Measured on an M2 Max, release build, SQLite's default 2 MiB page cache, 300,000 events over 100 days and a retention of 10 days, so 270,000 expire: with every event in one issue a sweep deleted about 18,000 events in its 100 ms and all were gone after 15 sweeps. With the events spread round robin over 1,000 issues, the worst case, a sweep deleted about 870 events, the longest sweep took 142 ms and its commit 50 ms more, and the backlog took 311 sweeps. Each issue a batch touches costs about 0.3 ms of page reads. With a 128 MiB page cache the spread case ran three times as fast. These runs used batches of 200 events, before the work bound, which has not been measured again. The migration's backfill took 360 ms over the 300,000 events.

### Logs

All projects share one Tantivy index per UTC day, so a project's own retention cannot delete a directory.

- A day older than every retention, default and projects, is renamed into the trash whole, as before. While no project has a setting of its own, that is all retention does.
- A younger day loses the logs of each project whose retention it outlived: one `delete_query` for those projects, or for every project but those that keep it when the day outlived the default, then a commit, then a merge of exactly the segments that hold deletes, so the logs leave the disk and not only the results. A day left with no logs is deleted. The log writer closes that day's own writer first, because an index has one writer at a time.
- The log writer reads the projects' settings from SQLite at start, before the first batch, and every minute after, then runs the pass. Before it deletes from a day it takes `retention::Lock`, reads the settings again and works out the day's cut anew, and it lets go once the deletion is committed, before the merge. A pass that cannot read the settings, at its start or before a day, deletes nothing more and reports it on stderr. A pass that fails is tried again 5 s later, then after 10, 20 and 40 s, and every minute after that. Until the settings are read, a late log is judged by the server default, so after a start that cannot read them this lasts seconds, not a minute. Reading no settings as "no project has one" would delete every day older than the default, the logs of projects that keep them longer included. It remembers what it cut from each day, so a day is read again only when a setting changes or the day outlives another retention. A pass that has cut or removed days for longer than 1 s stops after that day, and the next goes on. A day that fails, an index that does not open for instance, is reported on stderr and skipped, so one broken day cannot stop the retention of every day after it. Each pass first empties `.trash` of what a crash left between renaming a day there and removing it.
- Ingest waits while a day is cut, and the queue of 50,000 records absorbs that. Measured on an M2 Max: cutting half of a day of about 1,000,000 logs (176 MB) took 4.4 s, and a day of 120 MB 3.4 s. A shipper that sends more than the queue holds in that time gets `429` and retries. A cut happens once per day and project with a shorter retention, and after a change. A log written to a day makes the next pass check that day again: it opens the index and counts, and writes nothing when nothing expired, so a shipper that backfills an old day has it checked every minute while it lasts. Moving the cut off the log ingest thread is deferred work.
- A log that arrives late is judged by its own project's retention: in a project that keeps 7 days, a log stamped 10 days ago takes the time it arrived and keeps the original in `davy.original_timestamp`, as every project did under the server default, so it is not deleted the moment it is written.

### Discarding an issue

An issue that only wastes space, such as a bot hitting a removed route, can be discarded: Davy stops storing its new events and counts them instead.

- `davy issue discard ID --data DATA [--undo]` sets `issues.discarded_at` to now, or keeps the first time when the issue is discarded already, and prints the issue as JSON, `{"issue":7,"project":"shop","title":"payment declined","discarded":true,"discarded_at":"2026-11-25T10:00:00Z"}`. `--undo` clears the mark, and the next event is stored again. An issue left with no event and no feedback is forgotten at `--undo`, as retention forgets it, so a later event of the error opens a new issue. An unknown ID fails with `no issue ID`.
- The mark shows wherever the issue does: `discarded_at` in `davy issues --json`, `davy context` and the MCP tool `issue_context`, `(discarded)` in the `davy issues` text view, and a faint `discarded` beside the title in the web UI. `davy issues --discarded` lists the discarded issues. Over MCP an agent lists them with `sql`, `SELECT id, title FROM issues WHERE discarded_at IS NOT NULL`.
- JSON that Davy prints at the CLI keeps titles and messages exact but writes C1 controls (U+0080 to U+009F), DEL and the bidi marks (U+061C, U+200E, U+200F, U+202A to U+202E, U+2066 to U+2069) as `\uXXXX` escapes, so an event's title cannot move a terminal's cursor or reorder what it shows. The text views replace them, as they replace C0 controls.
- The writer groups each event as before. When the event's issue is discarded, it adds one to `discarded_daily(day, project_id, records)` for the UTC day Davy received it and stores nothing else: no event, no payload, no count in `issue_hourly`, no tag and no alert. The check is one lookup by the unique key of `issues`, so ingest does no extra work for an issue that is not discarded. A new error has no issue yet, so it is never discarded.
- An issue of Davy's own project can be discarded like any other: a crash is queued to the same writer as a client's event and meets the same check, so a panic that repeats is counted and its crash file deleted, and as none of it is stored, `project_status` and `davy doctor` do not show it.
- The events stored before the mark stay, and retention deletes them as usual. Retention keeps the issue itself once its last event expired, with an `event_count` of 0, so the mark holds for the events that come later.
- `davy usage` shows the counts per project as `errors_discarded` over the window. They count events received, not distinct events: an SDK that resends an event after a timeout is counted once per send, as Davy keeps no event id of a discarded issue to tell a resend apart. A resend of an event stored before the mark is still dropped as a duplicate and not counted. The writer's sweep keeps 400 days of them, as it keeps `usage_daily`, at most 10,000 rows a sweep.
- Only a person discards, at the CLI, and later in the UI. No MCP tool sets the mark: an agent's prompt can carry text someone else wrote, such as an event's message, and a discarded issue hides every later event of it. The `sql` tool opens the database read-only, so it cannot set the mark either. Ingest never writes the column: a field named `discarded_at` in an event's payload is payload. The savings view only prints the command for a person to run.
- Migration 23 adds the column and the table, with the `searches` and `search_days` tables the savings view reads, in one entry.

## Log patterns

An agent asks what fills a project's logs and what it could drop. Patterns answer that: a line's pattern is the line with its values masked, and Davy counts records and bytes per pattern.

### The mask (pure function, versioned)

`logs::patterns::of(message) -> Pattern { id, template }` in `src/logs/patterns.rs`. There is no clustering: a line's pattern depends on the line alone, so a line has the same id in every process, on every day and after a restart, and an id an agent noted yesterday still finds today's lines.

- It reads the first 4 KiB of the message, which is already scrubbed, and splits it into tokens at whitespace. Inside braces a comma ends a token too, so compact JSON such as `{"a":1,"b":2}` has one token per key. Only the first 12 tokens count, because the start of a line names what happened and the tail is mostly values. A token is cut at 64 bytes and ends in `...`.
- What is masked: UUIDs become `<UUID>`. IPv4 with or without a port, IPv6 and the scrubber's `[Filtered: ip]` become `<IP>`. E-mail addresses and `[Filtered: email]` become `<EMAIL>`. RFC 3339 times become `<TS>`. `0x` hex and words of 8 or more hex digits with a digit among them become `<HEX>`. Numbers become `<NUM>`: a run of numbers and separators such as `24/Nov/2026:10:23:45` folds into as few as it can, and a number with a unit, `23ms` or `4194304kB`, keeps the unit. Other words that mix letters and digits become `<ID>`. Quoted strings become `<STR>`.
- Words without a digit stay, so `deadbeef` stays and `user_id=8812` keeps its key. Other scrub markers stay as they are.
- An e-mail address, an IPv6 address and a time are masked only when they are the whole token, so `from=<alice@example.com>` and `token=hunterTwoSecret` stay. A template is log text, as free of personal data as the scrubbed logs and no more, and the `log_patterns` description says so and calls templates and examples data, not instructions.
- Two kinds of quoted string keep their words, masked one by one: the value of the JSON key `msg`, `message`, `error`, `err` or `event`, which is the prose of a JSON line, and an HTTP request line such as `"GET /api/users/123 HTTP/1.1"`, which names the route.
- A quote opens a string only after a byte that is not a letter, a digit or a backslash, and only when it closes, so `user's` and `can't` stay words.
- The id is the first 48 bits of blake3 over the mask version and the template, printed as 12 hex digits. 0 is never a pattern's id: it is `other`, below.

`patterns_tests.rs` has lines in the shape of nginx access logs, JSON and key=value app logs, syslog, Java, Spark and HDFS, written for the test, and the template each must give.

### Mask versions

- **Pattern ids are defined per mask version.** `patterns::MASK` is 1, and the hash prefix is built from it: the bytes `v1` and a zero byte, the bytes ids were made with before the version had a number, so no id changed when it got one.
- **The golden file pins mask 1.** `src/logs/patterns_v1.golden` holds one JSON line per case, `{"kind", "message", "template", "id"}`: every mask kind (numbers, units, hex, UUIDs, e-mails, IPv4, IPv6, times, quoted values, JSON keys, prose keys, request lines, scrubber markers), the cuts at 12 tokens, 64 bytes and 4 KiB, and the edges that depend on jiff's time parsing and on `Ipv6Addr`: offsets, fractions, bare dates, zone ids, `::`, IPv4-mapped and scoped addresses, plus Unicode, control characters and long lines. `mask_v1_matches_its_golden_file` fails on any difference and says to add a mask version. A line may be added when the current code produces it. A line is never edited, so a dependency update that changes a parse fails CI there.
- **Any change to the masker bumps the version.** Its ids are then new: an id an agent noted finds only lines of the old mask. The new version gets a golden file of its own, and the old one stays as long as its mask is in the binary.

#### When mask v2 is planned

Drop rules (see [Drop rules](#drop-rules)) name lines by pattern id under the mask they were made with, so a bump no longer costs only the ids agents noted. No masker change ships without this plan, or at least a forecast that adds back what rules of the old mask no longer drop, a `davy doctor` failure on enforced rules of the old mask, and upgrade notes that say so:

1. Mask 1 stays in the binary as a frozen module beside mask 2. The log writer runs it only for projects that have rules of mask 1, so enforced rules keep dropping exactly what was approved.
2. Rule listings and `davy doctor` show such rules as "runs on mask 1", a warning.
3. For each mask 1 rule, the writer records the mask 2 ids and templates of the lines it matched: the top 8 with counts.
4. A rebase creates shadow rules for them, never enforced ones, because a mask 2 template can cover lines the mask 1 one did not. A mask 2 rule in shadow next to the rule it replaces reads about 0 when the two match the same lines.
5. Mask 1 leaves the binary some releases later. Before that release `davy doctor` fails on rules still on mask 1. In it, they are turned off by the server with an audit row.

### Counters

Per UTC day, project, pattern, service and level, Davy counts records and raw bytes. Per day, project and pattern it keeps the template and one example, the first message it counted, cut to 256 bytes. The message is scrubbed before it is counted, so an example holds nothing the logs do not. Control characters and the marks that reorder text (U+061C, U+200E, U+200F, U+202A to U+202E, U+2066 to U+2069) in a template, an example or a service name are kept as U+FFFD, because they reach agents and terminals, also as JSON, which escapes only C0. `davy logs --patterns` replaces the same characters in what it prints, as the CLI does wherever it shows text from outside.

- Raw bytes are the message, the service, trace and span ids, and the attributes as compact JSON: about what a shipper sends for the record, before Davy compresses it.
- The log writer counts each record as it indexes it, and stamps the record with the id it counted it under, in the indexed field `pattern`. It decides overflow at that moment, so a record counted under `other` is found under `other`, and the report and `count_logs` with a pattern agree.
- **A file per project in each day index, not SQLite.** A project's counters of a day live in `patterns-<project>.json` in the day's directory, written whole and synced to disk (a temporary file of a new name, then a rename and a sync of the directory, so a power loss leaves the old file or the new one) when they changed. A flush runs every 10 s and rewrites only the projects that changed, and a project's file waits at least 1 s per 10 KB of its size before it is written again: a file of 1 MB at most every 100 s, so a sender that fills a project's bounds and then sends a line every few seconds costs at most 10 KB/s of writes, not a rewrite of every project's counters. Closing a day writes nothing: the writer keeps its counters, the flush writes them at their pace, and the day goes once it is written and its pace is over. A sender that sends to one day after another, so each line closes a day and opens another, made a file of 2.68 MB be written 25 times in 100 lines when closing wrote it (measured). The writer keeps at most 3 days it closed. Past them the one closed longest ago goes, a day already written first, and a day not written yet is written then whatever its pace. The files are written outside the lock readers take. A file that cannot be written stays in memory and is tried again at its next turn, and only the first failure of a run is reported. At shutdown the writer closes every day, whatever fails on the way, and writes the counters of every day it holds. They have the partitioning and the life of the logs they count. Retention that removes a day removes its counters. A cut of projects from a day removes their files under the same retention lock and settings as the deletion of their logs, right after that deletion commits and before the merge that takes the logs off the disk. The merge runs whether or not the files went. The writer tries a failed cut of counters again on every pass until it holds, the same cut whatever the settings say by then, since the logs it counts are gone under any setting. A log written to that day drops its failed cuts, since they would take the counts of the new log too. After a restart a pass cuts the counters of the projects the settings of that moment cut: a pass that finds no log left to delete still merges a day that holds deleted logs and still removes the files. So a counter outlives its logs only until a pass can remove its file, unless the writer restarts or a log comes into the day before then and the project's retention was lengthened in between: then its counters stay until the day passes the new retention. In SQLite the counters would be one more job per batch in the one writer queue that error ingest waits on, a migration, and a second retention pass that must stay in step with the day indexes. SQLite would let `sql` query them. The tool and the command below answer what an agent asks.
- While the writer has a day open its counters are in memory. Readers in the server process, MCP among them, read that memory, so a report is as fresh as search. The CLI, in a process of its own, reads the files, at most 10 s behind for a small project and longer for a big one. A record is counted when the writer indexes it, but the count stays pending until the commit of its day holds: a failed commit takes the counts with the logs it lost, and neither a reader nor the file sees a pending count. A crash loses the counts since a project's file was last written, 10 s or more for a big file, while the logs lose at most 1 s, so after a crash the counters can show fewer records than the logs. They can show more only when the logs lose a commit the counters already hold, such as a day index restored from an older backup than its counters.
- A file is read only within the bounds below and at most 64 MB, and sums saturate at the largest count. A file that cannot be read does not fail a report: the report leaves that day's file out and names the day in `unread`. Only a regular file is read: a symlink is not followed and a FIFO does not hold up the reader. A report reads only the directories of days, not a symlink named like one, as search and retention do. The writer counts the project of a file it cannot read from nothing that day. A damaged file, one that is not counters Davy wrote, it renames to `patterns-<project>.json.unread-<time>`, which no reader takes for counters and a retention cut removes with the project's other files. A file the disk or its rights kept from being read it leaves alone and never writes over, so an error that passes, such as a permission an operator fixes, costs that day's new counts and not the old ones. It never writes over a file it cannot rename either, and writes no file of a day whose directory it cannot list, because it cannot tell which files hold counters. A report names in `restarted` the days whose counters started again from nothing, those with a copy set aside and those the writer holds that it started again, so an agent knows those days count fewer lines than they hold. Opening a day also removes the temporary files a crash left there.
- Logs indexed before this version have no pattern and no counts. A day index created before it keeps its schema for the rest of that day, so its new records are neither stamped nor counted: a report of that day would list ids that find none of them.

### Bounds

Logs can come from a public DSN, so one sender could make every line a pattern of its own.

- At most 1,000 patterns per project and day. A record of a new pattern past that counts under `other`, id 0. A pattern counted before keeps counting as itself.
- The first 500 patterns of a project and day are counted as they come. Past them a new pattern counts under `other` until it was seen 3 times that day, so lines seen once, junk or values the mask missed, cannot take the rest of the room from lines that recur: a sender who wants the room must send each pattern 3 times. Its sightings are kept for at most 1,000 new patterns. When that table is full, each pattern in it loses a sighting and those left with none go, as in the Misra-Gries count of frequent items: a pattern more frequent than one new line in a thousand gets in whatever junk comes between, and the table costs at most one step per sighting on average. The sightings before a pattern gets in stay under `other`, where they were counted and are found. The report names `other` `(patterns without a count of their own: past 500 of a project in a day until seen 3 times, past 1000 always)`.
- At most 10,000 pairs of pattern and service per project and day. A new service past that counts under `(other services)`. A service name is cut to 128 bytes.
- A template is at most 12 tokens of 64 bytes and an example 256 bytes. So a project's day costs at most about 1 MB for its patterns and 2.5 MB for its pairs, and the writer holds only the days it has open, at most 3, the days it closed, at most 3 more, and those it failed to write. A sender that sends to more than 6 days in turn makes a day be written on each switch, past its pace.

### Reading

- `search_logs` and `count_logs` take `pattern`: 12 hex digits, or `other`. The CLI takes `davy logs --pattern <id>`. In a day without the field, nothing matches.
- The MCP tool `log_patterns` and `davy logs --patterns [--json]` take `project?`, `since?` (default 24 hours), `until?`, `level?` (the lowest level) and `limit?` (default 20 for the tool and 100 for the CLI, at most 100). They answer `{from, to, records, bytes, patterns, top}`, `unread` when a day's counters could not be read, `restarted` when a day's counters started again from nothing, and `truncated` when the window held more than a report reads, with `patterns` the number of patterns in the window and `top` the rows with the most bytes first, each `{id, template, example, records, bytes, share, levels, services}`. Sizes are strings such as `1.2 MB`, `share` is a percent of the window's bytes with one decimal, `levels` the records per level with the empty ones left out, and `services` the top 3 by records and one `(N more)` entry for the rest. The counters are per day, so the window is whole UTC days, from `from` to `to`.
- A report reads at most the newest 92 days of its window, and reads no further day once it has merged 50,000 patterns and pairs of pattern and service, about five days of a project that fills its bounds. Then `truncated` is true and `from` is the oldest day it counted. So a report costs at most about the merge of 50,000 entries and one more day, whatever the window and whatever a sender did to the bounds.

### Overhead

Masking and counting run in the log writer thread, once per record, after the scrubber and before the index. Ingest answers 202 before that, so they cost writer throughput, not request latency.

Measured with `examples/fill_logs DATA 300000 24 4` (300,000 records of the made-up API over 24 hours in 4 projects, through `LogWriter`, wall time including the last commit), on an Apple M2 Max with load average 8 to 10. Six rounds, alternating which binary ran first, built from the commit before patterns and from this one:

| | median | fastest |
| --- | --- | --- |
| before | 11.80 µs per record | 11.44 µs |
| with patterns | 12.02 µs per record | 11.55 µs |

The rounds differ from each other by up to 1.5 µs, so the cost, about 0.2 µs per record (2%), is near the noise. The masker reads at most 4 KiB and 12 tokens of a message, so its cost has a ceiling however long the message is. Counting the raw bytes serializes the attributes once more, which grows with their size.

### Not in this feature

A view in the web UI. Rules that drop a pattern at ingest came later, see [Drop rules](#drop-rules), and so did when a pattern was last searched, see [Last queried](#last-queried).

## Hardening

A security review of `370e4cd` showed that a client without a key could make the server allocate 4.4 GB with one 194 KB request (measured with a release build on a developer laptop: a gzip body that inflates to 190 MiB of `[0,0,...]` sent to `/store/`). The rules below close that class. Each has a test that a request over the limit is refused.

### Authentication before work

- A Sentry request with the key in `X-Sentry-Auth` or `?sentry_key=` is checked before the body is read.
- `/store/` has no envelope header, so a `/store/` request without a header or query key is a `401` before the body is read.
- An envelope without a header or query key is read only until its first line decompresses: at most 128 KiB of the body, plus the frame that crosses that mark. Davy decompresses at most 64 KiB of that line, parses it as the envelope header and checks the key in its `dsn` against the project in the path. Only then does it read the rest. A header line over 64 KiB, or none in the first 128 KiB, is a `400`. Each try decompresses from the start, so a new try waits until the body has doubled, and a body sent a byte at a time costs a few tries, not one per byte.
- Logtail, OTLP and Vercel drains check the bearer token before reading the body, as before. A Vercel drain to a project with a drain secret also needs a well-formed `x-vercel-signature` before the body is read, and its HMAC is checked over the bytes as sent, before the body is decompressed or parsed.

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
| `user.id` of an event | 256 characters, as for the device id of a session | the event is kept without it |
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
`set_retention` to eleven, `log_patterns` to twelve and `usage` to thirteen, and `drop_rules`, `drop_rule_shadow` and `drop_rule_remove` (see [Drop rules](#drop-rules)) brought it to sixteen, and `drop_rule_live` (see [Agent tokens](#agent-tokens)) to seventeen, and `ledger` (see [Ledger](#ledger)) to eighteen, of which the admin token lists seventeen. `retention` in `project_status` shows the days
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

## Usage and disk forecast

An operator or an agent asks three questions: what each project sends Davy, how fast it grows, and when the disk fills. Davy answers in resources, bytes and days, through the MCP tool `usage`, `davy usage` and a line of `davy doctor`.

### What is counted

- Migration 18 adds `usage_daily(day TEXT, project_id INTEGER, signal TEXT, records INTEGER, bytes INTEGER, PRIMARY KEY(day, project_id, signal)) WITHOUT ROWID`. `day` is the UTC day Davy accepted the data, as `YYYY-MM-DD`, the name of the log day directories. The signals are `errors`, `logs`, `sessions`, `feedback` and `checkins`.
- A record is one error event, one log record, one session or session aggregate item, one feedback item or one check-in. Client reports are not counted. An event sent twice with the same id counts twice, as both were received.
- `bytes` is the size of the item after decompression, as the SDK or shipper sent it: the envelope item's payload, the whole body of `/store/`, of a Logtail request or of an OTLP request. Scrubbing runs after it is measured. It replaces values with markers of about the same length, so the stored size differs by those markers only. `bytes` says how much data a project sends, not what it takes on disk: Tantivy and SQLite compress, index and keep payloads of only some events.
- Data is counted when it was accepted: as soon as the writer queue or the log queue took it, before the writer answers, so a request whose client hangs up after that still counts. A refused request counts nothing, and when a full queue refuses the later parts of an envelope, only the parts queued before count. The counts are what Davy received and accepted, not what it stored: the writer can still turn down an accepted item, such as a check-in it refuses or a session it skips, and a write that fails still counts.
- Ingest adds to a map in memory, keyed by day, project and signal. A project is one that a key authenticated and the signals are fixed, so the map holds at most a few entries per project. Every 5 s a task moves the map into `Job::Usage` jobs of at most 500 rows and waits until the writer commits each one. The writer runs every usage job in a transaction of its own, so a failed count write cannot roll back ingest in the same batch, nor ingest the counts. Counts the writer did not commit, because its queue was full or the write failed, go back into the map for the next flush. On shutdown the server stops the 5 s task between two sends, so a job it sent is answered and one that failed goes back into the map, flushes once more after the last request ended, and prints the error if that last flush fails. A crash loses the counts not committed yet: those of the last 5 s, or more while the writer is behind.
- The writer's sweep keeps the rows of the last 400 days, today included, so a year can be compared with the one before it. One sweep deletes at most 10,000 older rows, so the first sweep after a long outage holds the writer only briefly, and the next sweeps delete the rest.
### What is counted: stored logs, cut pairs, SQLite size

`usage_daily` holds what clients sent. A forecast of the disk needs what Davy stored, which pairs of day and project retention cut, and how SQLite grows. None of it can be rebuilt later, so migration 20 counts it from now on:

- `log_days(day TEXT, project_id INTEGER, records INTEGER, bytes INTEGER, cut INTEGER NOT NULL DEFAULT 0 CHECK(cut IN (0, 1)), PRIMARY KEY(day, project_id)) WITHOUT ROWID`.
- `store_days(day TEXT PRIMARY KEY, at INTEGER, bytes INTEGER, events INTEGER) WITHOUT ROWID`.

`log_days` counts the logs the writer stored:

- `day` is the index day: the record's timestamp after the writer moved one outside the window to when it was observed, so the day directory the disk walk measures. `usage_daily.day` stays the day Davy accepted the data.
- `bytes` are raw bytes as the pattern counters count them: the message, service, trace and span ids, and the attributes as compact JSON. They are counted for every stored line, on every day index, also on one created before the `pattern` field.
- A count is pending until the commit of its day holds, like the pattern counters. A failed commit takes the counts with the logs. Settled counts wait in memory, and the 5 s usage flusher sends them to the SQLite writer as `Job::LogDays`, at most 500 rows a job, which adds them to the rows in a transaction of its own. A job the writer did not commit goes back for the next flush. On shutdown the server flushes once before it stops the log writer, as that stop can take seconds and a kill then would lose the counts, and once after it, so the counts of its last commit go out too. A crash loses the last 5 s, as for `usage_daily`.
- The first job of a server carries when its log writer started counting. The writer keeps the first one ever as `firsts('log_days')`, in ms, which the setup page does not read. A day is whole when its start, 00:00 UTC, is at or after it.
- `cut` is 1 once retention deleted the pair's logs, and never goes back to 0. Retention queues the marks with the counts, in order, and the writer applies each job in that order, so a mark never lands before a count that came before it. A count that arrives for a pair already cut keeps `cut` at 1.
  - After a deletion from a day committed, under the retention lock and with the cut it deleted by, never a setting read before the lock: `Cut::Only(ps)` marks the projects in `ps`, and `Cut::AllBut(keep)` every row of that day but those of `keep`. A pass that finds the logs of the cut gone already marks them too, which restores a mark a crash lost for any day still on the disk: the first pass of a server checks every day again. It does so once per server, day and cut, so a day that new logs send back to the pass does not queue the same mark every minute.
  - A day removed whole, by retention or because its last log went, marks every row of the day once it was renamed into the trash.
  - The first pass of a server that lists the log directory without an error marks every row of a day that has no directory, nor a stray in its place. Those logs are gone whatever the settings say, so a mark a crash lost after a whole day went is restored too, even after a retention raise while the server was down. A listing with no day at all marks nothing: a restore that brought SQLite back before the logs, or a mount that is missing, looks the same as logs removed by hand, and a mark is never taken back. The first start that finds a day marks the rest.
- The sweep that prunes `usage_daily` keeps 400 days of `log_days` and `store_days`, at most 10,000 rows of each a sweep. A `log_days` row older than that goes only once it is cut, as a project may keep its logs up to 36,500 days. Log retention never deletes a row, it only sets `cut`, so the stored rate per day does not depend on retention. The prune and the sample run in a savepoint of the sweep's transaction: a failure is printed and rolled back alone, so it does not roll back the sweep's deletes or the jobs batched with it.

`store_days` samples SQLite once a day: the SQLite writer's sweep writes one row on its first run of each UTC day, after it deleted what expired. `bytes` is `(page_count - freelist_count) * page_size`, the live pages: space retention freed is reused, so the difference between two days is net growth. `events` is `SUM(issues.event_count)`, the events kept now, read from the covering index `issues_status_sorts`, so no event row is read.

`usage` and `davy usage` show `logs_stored` per project: records and raw bytes from `log_days` on the index days of the window, and per day. `logs.bytes` is what clients sent, decompressed, by accept day. `logs_stored` is raw bytes as the pattern counters count them, by index day. They measure different things and are never subtracted from each other. `logs_filtered` is the lines live drop rules dropped, by day of receipt: in `logs`, never in `logs_stored`.

Limits: a mark is set on a whole pair, so logs written to a day after retention cut it, such as a late log of a project that keeps fewer days, leave the pair marked. A pair whose logs a crash took between the deletion and the flush, whose day is still on the disk and no longer past its project's retention after a raise, stays unmarked. The forecast below reads these counts.

### Disk

Measured when asked, by walking the data directory. Sizes are allocated blocks, what the files take on the disk. Measured on an M2 Max with an APFS SSD, a release build and other builds running: 100,020 files in 31 directories took 2.0 s on the first walk and 0.25 s on the next two. A walk of 1,000,000 entries should take about ten times as long warm (an estimate, not measured), and longer on a cold spinning disk. The time limit below, not this estimate, bounds a walk.

The walk reads only what Davy owns, and only so much of it:

- It never follows a symbolic link. Each directory is opened relative to its parent with `O_NOFOLLOW`, so a directory swapped for a link during the walk is not entered, and a link at `logs` is not walked. The data directory itself may be a link.
- It stays on the filesystem of the data directory, which is the filesystem the forecast is about. A directory on another filesystem, such as a mount at `logs`, is not walked, and the measurement names it as left out.
- It reads at most 1,000,000 entries for at most 20 s, and enters at most 32 levels of directories. It reads the clock every 256 entries, so a single read that hangs, as on a stale network mount, holds the walk past 20 s.
- What it may not read, such as `lost+found` on an ext4 mount, what lies past a limit and what is not walked for the reasons above it leaves out, and the measurement is partial: `disk.partial` names the first 10 paths left out with why, such as `lost+found: permission denied`, counts them all in `skipped_count`, and says in `stopped` why the walk ended early. The sizes do not hold what was left out. A path left out beside `logs` leaves the forecast alone: it takes what is not logs as constant, and `used` comes from the filesystem. A path left out under `logs`, `logs` itself, or a walk that stopped early gives no forecast, because the growth of logs is then unknown, and `no_forecast` says so.
- Any other failure, such as a data directory it cannot open, answers `disk_error` in place of `disk` and `davy`, and the counts come back all the same.
- The server measures at most once a minute: the MCP tool `usage` answers from the last measurement while its walk ended less than 60 s ago and it is of the same UTC day, a failed one included. The age counts from the end of the walk, so a walk slower than a minute, as on a slow network mount, still answers the callers of the minute after it instead of making each of them start another. One walk runs at a time. A caller that arrives during it waits for that walk, not for one of its own, and waits as a task, so it holds no thread that ingest needs. The walk runs on one blocking thread in a task of its own, so a caller that gives up does not stop it and the next caller gets its result. A caller waits at most 30 s, then answers `disk_error` "the disk walk is still running after 30 s, ask again later": the 20 s limit is checked between entries, so a read that hangs on a stale mount would otherwise hold every caller. `davy usage` and `davy doctor` walk each time they run.
- A log day is a directory with a day's name. A file or link with that name counts as `other`, not as a closed day.
- Systems without `openat` walk by path. They keep the limits and the partial measurement, but can follow a directory swapped for a link during the walk, size files by length rather than allocated blocks, and cannot tell another filesystem.

- `logs`: each day directory under `logs`, closed days and today apart. A day after today counts with today.
- `sqlite`: `davy.sqlite` with its `-wal` and `-shm` files.
- `uploads`: `sourcemaps` and `chunks`.
- `other`: everything else under the data directory, the log trash included.
- The filesystem's total and free space, from `statvfs` through `fs4`. `used` is total minus free, so it counts files outside Davy too.

### Forecast

The forecast is a pure function of the sizes above, each project's log and error retention, the error events each project received and SQLite's daily size. Logs of all projects share one index per UTC day, so Davy measures how fast all logs together grow on disk, not one project's share: the counts in `usage_daily` are bytes received, which say nothing about bytes on disk. It gives two answers: the bound, which takes every project's logs as kept the longest log retention and decides `days_left`, `full_on` and `davy doctor`'s failure, and the estimate, which keeps each project's logs as long as that project keeps them.

Davy's own log lines are left out of the forecast, as they are few: measured, 191 raw bytes on a quiet server's first day, the one line that says Davy made its project, and none in the 30 minutes after or across a restart. Counted as logs, that line would take a server that stores only errors off `errors_only` within seconds of its start. So the forecast reads no raw bytes of Davy's own project, a closed day whose raw bytes are all its own is no log day, and its 30 days of logs set neither the bound's longest retention nor a part of the estimate. `davy usage` still counts its lines under it.

- **Growth** is how many bytes of logs reach the disk per day. Davy reads it from raw bytes, which `log_days` counts from the first hour, times how they pack on disk: the ratio of a closed day's size on disk to its raw bytes. A day counts for the ratio when it is whole, no retention cut a project on it, it holds at least 10 MB of raw logs, below which the fixed cost of a day index dominates, and the walk found its directory. Davy takes the newest such days of the last 30 closed ones, so a server quiet at weekends still fills its count, up to `possible`: 7, or the shortest log retention of a project with raw bytes when that is less, as a server keeping logs 3 days never holds more than 3 closed days uncut. Without such a project the shortest of all projects, then the server default, decides. Raw bytes per day are read from the newest 7 whole closed days: the low and high end of their range, and as the middle the larger of their median and their mean, so a server busy on 3 days of 7, or back from days without logs, does not read as quiet. The high end is at least the middle. The growth has a basis, in this order:
  - `measured`: `possible` days counted for the ratio. The growth is the median size on disk of those days, and the ratio is shown for reference. When a whole closed day newer than the oldest of them had under 10 MB of raw logs, the server went quiet since, and the median would keep its busy rate for up to 30 days: the growth is then raw bytes per day times the ratio of those days, the blend below with `n = possible`. Raw bytes per day see the quiet days: one to three quiet days in 7, such as a weekend, leave the middle at the busy rate, from the 4th the middle falls with the mean, and on the 7th quiet day it is the quiet rate.
  - `measured_disk`, for a server upgraded from a version that did not count raw bytes: it never counted, or the closed days the direct method reads include one that ended before counting started and has no row in `log_days`, which only a version that counted writes. A line a counting version stored on an older day, such as one a shipper replays, writes a row, so it does not make a new server look upgraded. The day counting started on is not such a day, as a new server holds it too, and a new server blends from its first whole day. A directory older than the 30 days of rows Davy reads has no row and reads as upgraded. A server that keeps logs 1 day holds only the day counting started on the day after an upgrade, so it cannot be told from a new one and forecasts from its hours for that day. The growth is the median size on disk of the closed days no retention cut, as before raw bytes were counted, until the server's own days catch up. The same holds when no closed day counted for the ratio although whole days exist, such as a server with under 10 MB of logs a day; `why` names the small days once the server has counted `possible` whole days and none of them measured, and that it counted too few whole days before.
  - `blended`: 1 to `possible - 1` days counted. The ratio weighs the median of the server's own days `n` times and the footprint benchmark `possible - n` times, and divides by `possible`. Low and middle round down, high rounds up. The growth is raw bytes per day times that ratio.
  - `first_day`: no whole closed day yet, at least an hour counted and at least one raw byte. Raw bytes per day are every raw byte counted times a day divided by the time counted, and the growth that times the footprint benchmark, plus the bytes of one day index once a line was counted. One partial day says nothing of the daily cycle, so the range is a third to three times that rate under 24 hours counted, and two thirds to one and a half after.
  - `errors_only`: logs add nothing, as Davy counted no raw log bytes in the 30 days of `log_days` it reads and the disk holds no closed log day since the first day the direct method reads, and a project received error events in the last 7 closed days, or today for errors that began today. The logs' growth and room are 0, and the bound and the estimate run on the error parts alone. `why` says that no raw log bytes came in the last 30 days, so the logs add nothing and the forecast rests on errors. It is `errors_only` too when logs came but the log basis has no forecast for too little history, such as a first log line on an old server, one stray closed log day or under an hour counted, and errors grow: logs count as 0 until they give a forecast, as a forecast from errors understates and says so, while none would hide a disk that errors fill. `why` then says logs are not in this forecast yet and gives the log basis's reason, and the summary adds that the disk may fill earlier once they count.
  - Under an hour counted and no closed day on disk there is no forecast, and the reason says how many minutes were counted. A server whose logs add nothing and whose projects received no error events in the last 7 days has no forecast on any day, and the reason says nothing grows yet.
- **Range.** Each basis gives a low, middle and high growth. The range is the second lowest and second highest of the window with 5 or more values, so one odd day at each end does not set it, and the minimum and maximum with fewer. A blend takes the range of its own days and the benchmark's minimum and maximum. The forecast decides on the high growth for `blended` and `first_day`, which lean on the benchmark or on a few hours, and on the middle for `measured` and `measured_disk`, which rest on the server's own days, also where `measured` takes raw bytes per day. `full_on_range` runs the same forecast at the high and the low growth: the earliest day the disk fills and the latest, null for never or beyond 10 years. For `errors_only` it runs at the errors' high and low: the bound's error parts and the estimate's, so it is a single day while the footprint benchmark alone gives bytes per event.
- **Confidence** is `high` for `measured`, `medium` for `blended` and `measured_disk`, and `low` for `first_day`, for a walk that left something out, and while counting started within the last 7 days, unless the basis is `measured_disk`, which does not read the counts. When a walk that left something out or recent counting makes it `low`, `why` ends with that reason. `errors_only` takes its confidence from the errors alone: `high` once SQLite growth measured bytes per event on 7 days, `medium` while the footprint benchmark gives the rest, and `low` while errors that began less than a whole day ago, so their rate rests on hours, or less than 7 whole days ago add over 5% of the bound's error growth, in parts per million rounded up as the 5% rule below counts, while logs are left out, and for a walk that left something out. A new project with a few events next to a busy one thus leaves the confidence as it is. Raw log bytes counted for under 7 days do not lower it, as the errors do not read that count, and neither does the 5% rule below, as errors are all that grows. `why` ends with the reason for `medium` and `low`.
- **Errors** grow SQLite per project, until the project holds its error retention. A project's events per day are the mean of what it received over the newest 7 closed days since its errors began, as errors come in bursts. They begin on the first day `usage_daily` holds errors of the project, or when it was made if that is later, so an old project whose errors began yesterday counts one day, not seven. Until a whole day has closed since they began, it counts up to now, over at least an hour. Bytes per event come from SQLite's own growth: the sweep samples SQLite's live pages and the events kept once a UTC day, and two samples at most 48 hours apart make a day when the events kept grew by 10,000 or more and the pages did not shrink, bytes per event being the one over the other. Pages that retention frees are reused, so a day measures the net growth, and below 10,000 new events sessions, check-ins and freed pages weigh too much. The newest 7 such days of the last 30 count, `own_days`. Until there are 7, the footprint benchmark fills in: an event's row with its indexes and an hourly row of its own, 627 bytes, plus 0.5724 of what the project's events average as sent, the payload kept as JSON, rounded up. The estimate weighs what the measured days give a project `own_days` times and the project's benchmark `7 - own_days` times, over 7, so `errors.basis` is `measured` with 7, `blended` with 1 to 6 and `footprint` with none. SQLite grows by the events of all projects together, so what the measured days give a project is their median times its benchmark over the mean benchmark of all projects, each weighed by its events per day: a project with larger payloads takes more of it. The bound takes the highest of the project's benchmark, the measured days' high end, the second highest with 5 or more days and the highest with fewer, and that high end scaled by the project's benchmark over the mean as the estimate scales the median, so no project's bytes per event in the bound fall below the estimate's. Each project adds its events per day times bytes per event a day until it holds its events per day times its error retention in days; one that holds that many already adds nothing. A project that received no event in the window adds nothing, and without any the answer has no `errors`. When fewer than 7 days measured their bytes per event and the bound's error parts add over 5% of what all its parts add until the disk fills, `high` confidence falls to `medium` and `why` says so. Each part counts what it still adds, at most its room, so errors that already hold their retention weigh nothing, and errors that alone fill a disk whose logs hold theirs weigh all. Where the bound never fills, the error parts count what they add within the 10 years past which a day reads "beyond 10 years", each at most its room, as a share of the room left to the limit, with the same 5% and rounding up. The logs then do not weigh in: 10 events a day at 2,100 B kept 100 years add 76.65 MB in 10 years, 0.05% of 150 GB to the limit, and stay `high` whether the logs hold their retention or had a quiet week.
- **The footprint benchmark** is `bench/footprint.json` (see [Storage benchmark](storage-benchmark.md#footprint-for-the-forecast)). The binary carries its ratio, day index and bytes per error event as constants in `src/usage/footprint.rs`, and a test fails when they no longer match the file or one is 0. The answer names the run they come from.
- **Steady state** is the most retention lets Davy take: the growth the forecast decides on times one day more than the longest log retention of any project, plus what SQLite, uploads and other files take now, plus what each project's errors add until they hold its error retention. However the growth splits across projects, no project keeps its logs longer than the longest retention, and a retention of N days deletes a day once it is more than N days old, so the disk holds N closed days and today. Steady state is an upper bound for how the growth splits across projects. When projects keep logs for different times it is a worst case: the forecast says so (`worst_case`), names the shortest and longest retention, and at the current rate of logs the disk fills on `full_on` or later, or never. An agent reading it also holds `set_retention`, so the summary asks it to watch free space over a few days before it shortens a retention for that day.
- **Headroom** is 10% of the filesystem. Filesystems reserve blocks for root, and Tantivy merges and SQLite checkpoints need room for a copy of what they rewrite.
- Davy grows in parts: the logs by the whole growth until they reach their steady state, and each project's errors until they hold its error retention. When `used` plus what is left to steady state stays below total minus headroom, Davy never fills the disk at the current rate. Otherwise `days_left` is the least whole number of days at which `used` plus what each part added by then, at most what is left of it, reaches that limit, and `full_on` that UTC day. A what-if that lowers the longest log retention frees the closed days it deletes at the next sweep, so the bound takes them off `used` and off what Davy takes now. A day more than 3,650 days away reads "beyond 10 years": `days_left`, `full_on` and both days of `full_on_range` are null and `beyond_10_years` is true, as an answer that far out says more about the rate holding than about the disk. `davy doctor` takes it as never. A disk already past the limit has 0 days left while Davy still grows. When Davy no longer grows, because it is at or past its steady state or no logs arrive, logs never fill the disk, even one already past the limit: other files filled it, a shorter retention would not help, and the summary says to watch free space instead of a false "full today" that `davy doctor` would fail on every day. An `errors_only` forecast that never fills says errors never fill the disk at the current rate, as each project's error retention holds Davy at its steady state. Arithmetic saturates, so a huge growth means 1 day left, not an overflow.
- **The estimate** keeps each project's logs as long as that project keeps them. It splits the middle growth of the logs by each project's raw bytes per day, read as for all projects together: the larger of the median and the mean of the newest 7 whole closed days, or on a first day the bytes over the time counted. Each project's growth is so its raw bytes times one ratio for all projects, and the estimate takes every project's logs to pack alike, which they do not, as nginx and syslog lines differ by almost 2 times in the footprint benchmark. A project's room is its growth times one day more than its retention, less what it holds now: its raw bytes that retention did not cut, from its retention's first day on, on days whose directory is on disk, times the same ratio. A raised retention so does not count days it already cut. A what-if that shortens a retention frees what that retention deletes at the next sweep. Errors count at the estimate's bytes per event. The estimate is never earlier than the bound: where its sums put the day earlier, as when its ratio puts less on disk than the walk found, it takes the bound's day, or never, and `estimate.clamped` is true, as raw counts and the disk then disagree. Without raw bytes per project the logs are one part kept the longest retention, and a what-if that lowers it frees the closed days it deletes, as in the bound. The estimate is null for a what-if of `logs_bytes_per_day`, which Davy cannot split by project.
- A what-if changes one input and runs the same function: `logs_days` sets a project's log retention, or every project's without a project, which changes the forecast when it changes the longest retention. A what-if for one project that leaves another project holding the longest retention cannot change the worst case, and `what_if_note` says so instead of returning the same numbers without a word; the estimate shows what it does, unless Davy counted no raw bytes per project, which the note then says too. On `errors_only` logs count as 0, so a `logs_days` what-if, for a project or all, changes the forecast only by the closed log days it deletes, makes no worst case, and `what_if_note` says so. A what-if for a project needs `logs_days`. `logs_bytes_per_day` sets the daily log growth of all projects together and gives a forecast without history. It cannot name a project, because Davy cannot tell one project's bytes on disk. A what-if for an unknown project, a `logs_days` outside 1 to 36500 or a project with `logs_bytes_per_day` is an argument error, before the disk is walked.

Limits: the forecast assumes the rate of the last week holds. SQLite grows by error events alone: sessions, check-ins, feedback and monitors are taken as they are now, and the rows of an issue and its tags, which come once per issue rather than per event, are left out of bytes per event, so the benchmark's figure is about 38 bytes under its cold run at 20 events per issue, and lower still for errors that each open an issue, until the server's own days measure it. The benchmark keeps every payload, while sampling keeps fewer of an issue with many events, which errs early. The bound keeps the highest of the benchmark, the measured days' high end and that end scaled to the project's payloads, so its error growth does not come down with measurement: for a busy issue with payloads of 3 to 4 KB, which SQLite stores at about 650 bytes an event, it is 3 to 5 times early, and with fewer than 5 measured days the single highest day of the last 30 decides. While retention deletes as many events as arrive, the events kept do not grow and no day measures, so the benchmark decides. A project's events per day read `usage_daily` from the first day it holds errors of the project, so a server upgraded from a version without it counts them from the upgrade day on. A server whose logs add nothing forecasts from errors alone. Its first logs count as 0 until the log basis has a forecast: up to 2 closed days after a first log line on an old server, an hour after counting began on a new one, and only once retention cuts a single stray log day with nothing since. Until then the forecast errs late by what those logs add, and the log basis may then name another day. On `errors_only` `full_on_range` covers bytes per event, not the rate, so on the benchmark both ends are one day even at low confidence. Capping by the longest retention errs early when the projects with most logs keep them for less, and growing at the full rate until steady state errs early too. The median size on disk of `measured` and `measured_disk` covers up to 7 days, so a rate that rose in the last three days is not seen yet, and where every day of the 7 holds 10 MB or more raw, a server busy on 3 or fewer days of 7 reads at the rate of its other days. Raw bytes per day take the mean where it is above the median, so one burst day raises the growth by a seventh of the burst for 7 days, which errs early, and a server that went quiet reaches its quiet rate on the 7th quiet day. Other files on the filesystem are taken as constant. The footprint benchmark ran on one machine with six real log corpora, so a blend leans on how logs packed there until the server measured `possible` days of its own, and a first day's range is wide on purpose. Its raw bytes are the log line, while `log_days` also counts the service, the ids and the attributes, so for logs with parsed attributes the benchmark's ratio is too high and a blend or a first day errs early. `davy usage` and `davy doctor` read the database while `davy serve` runs and cannot see its flags, so they assume a server default of 30 days of logs unless given `--log-retention-days`. Right after the shortest log retention is raised, a `measured_disk` growth reads the closed days the old retention already cut, so it reads low for up to 7 days; the ratio skips days whose pairs were cut. A file with several hard links counts once per link: Davy makes none, so only a file someone else linked is counted twice, which makes the forecast earlier, never later.

### Interfaces

- MCP tool `usage`, with `project`, `since` (default `7d`, at most `90d`, as `release_health` reads it), `view` (`summary`, the default, or `savings`, see [The savings view](#the-savings-view)), `top` and `what_if`, such as `{"project":"shop","logs_days":14}` or `{"logs_bytes_per_day":2000000000}`, which takes no project. It answers compact JSON: `disk` (`used`, `free`, `total`, `full_on`, `days_left`, `beyond_10_years`, `worst_case`, `steady_state`, `headroom`, `headroom_reason`, `room_left`, what may still be used before total minus headroom, `past_limit`, true when the filesystem uses that much already, `partial`), `estimate` (`full_on`, `days_left`, `steady_state`, `beyond_10_years`, `clamped`, true when it took the bound's day; null for a what-if of `logs_bytes_per_day` and without a forecast), `davy` (what Davy's files take), `growth_per_day` (the growth the forecast decides on, the high end of the range for `blended` and `first_day`, 0 for `errors_only`), `longest_log_days`, `shortest_log_days`, `forecast` (the summary), `basis` (`confidence`, `full_on_range`, the range of the growth alone, null for never or beyond 10 years, so in a worst case both days are the earliest, `why`, with the reason when confidence is low, and `logs` with the `basis`, one of `measured`, `measured_disk`, `blended`, `first_day` and `errors_only`, `growth_per_day` low, middle and high, `own_days`, `possible_days`, `raw_per_day`, `raw_days`, `counted_hours`, `ratio`, `ratio_range`, `own_ratio`, `footprint_ratio` and `footprint`, the run the constants come from, and `errors`, null when no project received error events in the last 7 closed days, or today for a project whose errors began today, with `projects`, `events_per_day`, `bytes_per_event`, `own_days`, `basis`, `growth_per_day`, the bound's, what errors add a day before they hold their retention, and `estimate_growth_per_day`, the estimate's, with `bytes_per_event` being it over `events_per_day`, rounded down; null for a what-if of `logs_bytes_per_day`, which has no basis), `what_if` and `what_if_note`, `projects` with each signal's `records` and `bytes` over the window and per day, `logs_stored` with the `records` and raw `bytes` of the logs stored on the index days from the window's first day to tomorrow, as a line may be up to a day ahead, and per day (a line sent in the window with a time before it is in `projects` only), `errors_discarded`, the error events of discarded issues Davy counted on the window's UTC days instead of storing them (see [Discarding an issue](#discarding-an-issue)), `logs_filtered`, the records and raw bytes of the log lines live drop rules dropped on the window's UTC days of receipt (see [Drop rules](#drop-rules)), `limits`, the object `davy project quota --json` prints of the project on the UTC day open now whatever the window, with log sizes as strings (`errors` and `logs`, null without a quota, else `quota`, `today`, `percent` and `refusing`, errors in events and logs in bytes received, `today` being what the writer stored, up to 5 s behind; `warn_percent`; `spike.errors` and `spike.logs`, each with `setting`, `default`, `off` or `set`, `per_second`, null when off, and `refusing`; `resets_at`; `refused_today` with `davy_quota` and `davy_spike`; `refused_log_requests_today`; and `busy_refused_requests_today`; see [Quotas and spike protection](#quotas-and-spike-protection)). Here every `refusing` is what the running server's gate says now: a quota by the counts it holds in memory too, and a spike rate, which only the server knows, for at most 10 s after a burst. `busy_refused_requests_today_no_project` counts the busy `429`s refused before a key was read. The rest of `projects` is the project's retention, `window`, `default_log_days` when a project has no log retention of its own and the forecast assumed the server default for it, and, without a forecast, `no_forecast` with the reason, or `disk_error` in place of `disk` and `davy` when the walk failed. Sizes are strings such as `4.1 GB`, in powers of 1000. Counts are kept per UTC day, so the window starts at midnight UTC of the day `since` names, `30m` at 15:00 included, and records and bytes per day both divide by the time since that midnight. A `since` in the future is an error.
  A disk past total minus headroom by files Davy does not grow has `days_left` and `full_on` null, as nothing Davy does fills it, and `past_limit` true, so a client never reads that null as room to spare; the summary says so too.
  The tool and the web UI's usage routes share one limit of 4 usage reads at once, as one read can take seconds on a large store: past it the tool answers `too many usage reads at once, ask again in a second` as an error and the routes `429` with `Retry-After: 1`.
- `davy usage --data DATA [--project SLUG] [--since 7d] [--logs-days N] [--logs-bytes-per-day N] [--log-retention-days N] [--view summary|savings] [--top N] [--json]` prints the same as text, with a line per project that has a quota, a spike rate other than the default or a refusal today, such as `logs 1.2 GB of 2.0 GB today (60%), spike errors 4000/s (default), logs off, refused today 0 over quota, 0 over spike`, followed by the log requests refused unread and the busy requests when there are any, a `basis` line and, when the low and high growth fill the disk on different days, a `range` line, at the high and low growth of logs, or of errors for `errors_only`, "at the earliest" in a worst case, and the rate as "up to" where the forecast decides on the high growth, or as JSON with sizes in bytes. `--view savings` prints the savings view instead (see [The savings view](#the-savings-view)). It opens the database read-only, so it runs next to `davy serve`.
- `davy doctor` extends its disk line: it warns when the forecast puts the disk full in under 14 days and fails under 2, with the fix to shorten retention with `davy project retention` or add disk, `--logs N`, or `--errors N` for an `errors_only` forecast, which it judges like any other. The bound's days left decide; beyond 10 years counts as never. The estimate never raises the status: it is never earlier than the bound, so it could not warn where the bound does not. It only lets a worst case under 2 days warn instead of fail, as follows. The line names its day where it differs from the bound's, and says what it assumes: each project's logs kept as long as it keeps them in a worst case, else the middle growth of logs. A worst case says "at the earliest at the current rate, if every project kept logs N days", and while the disk has days left it only warns where the estimate puts the disk full later than the bound, never, or is missing, as the worst case may then never fill on its day; where the estimate lands on the bound's day, under 2 days fails. Its fix asks to watch free space over a few days first. A `first_day` forecast likewise only warns while the disk has days left, names the hours it rests on and low confidence, and its fix asks to run doctor again once a whole day has closed before a retention is shortened. `--log-retention-days` gives the server's default log retention, 30 when not given. The line names that default for projects without their own, except on `errors_only`, where logs count as 0. Without a forecast for too little history, as on a new server, it judges free space alone. Without one for any other reason it warns, and its fix fits the reason, because a shorter retention would delete logs and bring no forecast back: a walk that stopped at its limit asks to watch free space by hand, a part of `logs` left out asks for `logs` as a plain directory on the data directory's filesystem that doctor may read, and a database or disk it cannot read has no fix beyond what its error says. The fix to shorten retention or add disk comes only with low free space or few days left.

### The usage page

The web UI shows the same report and savings to the person who runs Davy, and what each project stored per day. Two routes behind the session serve it; both only read.

- `GET /ui/api/usage` takes the parameters of the MCP tool `usage` but `what_if`: `view` (`summary`, the default, or `savings`), `project`, `since` (default `7d`, at most `90d`) and `top` (1 to 20, default 3). It answers exactly what `davy usage --json` and `davy usage --view savings --json` print, sizes in bytes, from the same code. A `view` other than the two, a `top` outside 1 to 20 or a `since` the summary refuses is `400` with the reason, an unknown project `404` with `unknown project "slug"`. It takes the disk from the walk the MCP tool takes it from: the server keeps one, at most a minute old, for both, so a page and an agent together walk the data directory at most once a minute.
- `GET /ui/api/usage/days` takes `project` and `since` (default `30d`, at most `90d`) and answers `UsageDays`: `days`, each UTC day from the day `since` names to today, `names`, the slug of every project, oldest first, `projects`, the first 8 projects, oldest first, or the one asked for, with a list per count in the order of `days`, and `other`, the rest summed, with `projects`, how many it sums, and `top`, the 3 of them that stored the most bytes in the window with their `stored`, the most first, or null when there is no rest or one project was asked for. So the answer of a server with 300 projects is that of 9, and a page draws at most 9 bands; asked for one project, it answers that project's counts alone. It reads the counting tables, one range read each on their day key, and never walks the disk.
  - `stored`: raw bytes of the logs stored on the day of their index, from `log_days`, with a line indexed for tomorrow on today, plus the bytes of the errors, sessions, feedback and check-ins accepted that day, from `usage_daily`. Logs count what was stored, so a line a drop rule dropped is not in it; the other signals count what was accepted, as nothing else counts their bytes per day.
  - `accepted`: records of every signal, from `usage_daily`.
  - `filtered`: lines live drop rules dropped, from `drop_daily`.
  - `discarded`: error events of discarded issues, from `discarded_daily`.
  - `limited`, `refused`, `unreadable` and `sdk_dropped`, from `client_reports`: items Davy refused past a rate limit, a daily cap or a project's quota (`davy_rate`, `davy_daily_cap`, `davy_quota`, `davy_spike`), for another reason of its own after it counted them (any other `davy_` reason, such as `davy_unknown_monitor`), check-ins it could not read (`davy_too_large`, `davy_invalid`), and items the SDKs reported they dropped before sending (every reason without the prefix). Those are kept 90 days. Client reports in bytes or time, of the categories `attachment`, `log_byte`, `profile_duration`, `profile_duration_ui` and `metric_second`, count no items and are left out, so a dropped 5 KB log line counts once. Davy keeps no count of the requests ingest refuses with `429` for a full queue or memory budget, so they are in none of these; those it refuses past a quota or a spike rate are in `limited` ([Quotas and spike protection](#quotas-and-spike-protection)).
  - `accepted` holds `filtered`, `discarded` and `refused`: ingest counts an item once a queue takes it, and the writer then drops a line a live rule matches, an event of a discarded issue, a check-in for a monitor that does not exist. `limited` is partly in it: a check-in past its rate limit and a session or feedback past the daily cap were accepted, an item past a project's quota never was. `unreadable` and `sdk_dropped` are not in it: Davy counts a check-in only once it reads it, and an SDK's dropped item never arrived.
  - Both routes take a place among the usage reads the MCP tool `usage` takes them from, 4 at once, and answer `429` past them.

The page (`/ui/usage`, **Usage** in the header) is one column of five parts. The rail picks a project, or all of them, and `?project=` keeps the choice in the address. The rail, the legend and the projects table list the projects in one order, oldest first, from `names`.

- **Disk.** Used, free and total, the room left before the last 10% of the filesystem, which Davy keeps free, the day the disk fills ("never", "beyond 10 years" or "today" where it says so, with the range of days when the low and high growth differ), the steady state once every retention is full, and the forecast's basis and confidence with its `why`, or the `no_forecast` reason. From `GET /ui/api/usage` with the default window of 7 days, so it agrees with `davy usage`. A disk `past_limit` has no room left and fills "now", never "never", and then, as when a day is forecast, the page says what makes room: free space on the disk, a shorter retention, or the savings below.
- **Stored per day.** A stacked area of `stored` from `GET /ui/api/usage/days` with the picked `project`, 30 days or, with `?days=90`, 90. A project takes its hue from its place in `names`, so it keeps it when picked alone: the first eight take the eight series hues in a fixed order, and the rest share one gray band named "Other, N projects". A band's key is `p:` and the slug, and Other's is empty, so no slug can stand for Other. A band that stored nothing in the window is left out, and a chart with no band says "Nothing stored in these days." Today is still counting, so its segment is drawn lighter and dashed, only once it holds something, and its label says "today, so far", as does its row in the table: the chart never shows today's start as a fall. A legend names the bands. The pointer or the arrow keys pick a day, and a line under the chart, a polite live region, reads it out: the total, then each band in the order of the legend, then for Other the 3 projects that stored the most that day and how to see one alone. **Show table** gives the same numbers as a table, a row per day, newest first, with a column after Other for each of its top 3.
- **Projects.** Per project the records and bytes of each signal, logs stored, logs filtered by drop rules, errors discarded and the log and error retention, from the same report as the disk. When a project has a quota, a column **Quota today** shows today's use of it from `limits`, logs first, such as `logs 1.2 GB of 2.0 GB` and under it `errors 40 of 5,000`, with `, reached` at 100% or more; a project without one leaves the cell empty, and with no quota anywhere the column is not there.
- **Savings.** The entries of `davy usage --view savings` from `GET /ui/api/usage?view=savings`: log patterns with their share, bytes per day, when a search last asked for them, their example line and the server's `rule` in words with its `command`, then retention that fits, noisy issues and drop rules on trial, each with its command. A drop command needs one project, so with all projects the patterns say to pick one. The page parses no command and matches no text: it shows what the server sent, and runs none of it. A command or an MCP call with a character `shown()` escapes gets no copy button, and a line says why, as what it would copy is not what it shows. Under the entries a line says what "last queried" counts; the server's whole note, written for agents, is behind a disclosure.
- **What became of what was sent.** A table of each project, and Other with its top 3, per UTC day, newest first, from the same days answer as the chart. Its header groups the columns as they relate: accepted, then of those not stored, filtered, discarded and refused, then rate limited, which is partly accepted, then never accepted, unreadable and dropped by the SDKs. A line above it says so, and that client reports in bytes or seconds are left out. It shows only the rows where something arrived, of the newest 14 days unless every day is asked for, and draws at most 300 rows, with a line that says how many it left out.

Project slugs, pattern examples, issue titles and rule text go through `shown()` and render as text, as in [Untrusted text in the browser](#untrusted-text-in-the-browser). The chart is SVG drawn by the page, without a chart library, its eight hues pass a check for contrast against the page and for color vision deficiency in light and dark, and the page loads as its own chunk the first time it opens, so it adds nothing to the first visit but its route.

## Quotas and spike protection

One project that floods Davy, by a bug that logs in a loop or a release that throws on every request, must not fill the disk or take the queue from the others. A person sets a daily quota per project and signal, and Davy refuses what a project sends past it until midnight UTC. No quota is set by default: a server that one team runs for itself has no reason to refuse its own data until someone decides so. A spike rate of error events is on by default, set well above any normal workload, so one project cannot take the store writer from the others ([Spike protection](#spike-protection)).

### Settings

- Migration 25 adds to `projects` the columns `quota_errors INTEGER CHECK(quota_errors > 0)`, `quota_logs INTEGER CHECK(quota_logs > 0)`, `quota_warn INTEGER NOT NULL DEFAULT 80 CHECK(quota_warn BETWEEN 1 AND 100)`, `spike INTEGER CHECK(spike >= 0)` and `spike_logs INTEGER CHECK(spike_logs >= 0)`, the table `quota_alerts(day, project_id, signal, kind, quota)`, which marks each quota alert sent, and the table `refused_requests(day, project_id, reason, quantity)`, which counts requests refused before their body was read ([Refusals](#refusals)). Old data: every project created before it has no quota (`NULL`), warns at 80% once it gets one, and has `spike` and `spike_logs` `NULL`, the defaults. Nothing is rewritten.
- `davy project quota <slug>` prints the project's quotas, today's use, the percent of each quota, whether each signal is refusing now, its spike rates, when the day resets and today's refusals of each reason, for a person; `--json` prints it as JSON, numbers only. `--errors N` sets the error events a UTC day, `--logs SIZE` the log bytes, `off` removes either, `--warn PERCENT` (1 to 100, default 80) sets when an alert warns, `--spike RATE` and `--spike-logs RATE` set the spike rates of error events and of log records, `off` turns either off and `default` goes back ([Spike protection](#spike-protection)), and `--clear` removes all of it. A size needs its unit, `B`, `KB`, `MB`, `GB` or `TB` in powers of 1000 as Davy prints sizes, such as `2GB`, `500MB` or `1000B`: a bare `1000` is refused, as nobody means a thousand bytes a day by it. A size above 1 PB or below 1 B is refused with Davy's own message. A running server reads the settings every 5 s, so a change takes effect within 5 s without a restart, which the command says.
- No MCP tool changes a quota. An agent reads them in `usage`, per project under `limits` from the same code as `davy project quota`, and `davy usage` and the usage page show them too ([Usage and disk forecast](#usage-and-disk-forecast)); a person decides what a project may cost.

### What is counted

- Errors count in events, logs in bytes as received, decompressed. An error event costs about the same to group and store whatever its size, up to the 1 MiB item cap, and a person thinks of errors as a count ("5,000 errors a day"). A log record costs what its bytes cost: one line of 64 KiB takes as much disk as a thousand short ones, and a quota in lines would let a project that logs whole payloads fill the disk inside its count. Both are the numbers `usage_daily` already keeps, `records` of errors and `bytes` of logs, so the quota, the usage report and the forecast speak of the same amounts. Sessions, feedback and check-ins already have daily caps of their own ([Sizes and counts](#sizes-and-counts)).
- The day is the UTC day, as in `usage_daily`. The counts live in memory in the gate (`src/quota/gate.rs`), a map from project to today's error events and log bytes behind one lock. The usage counter adds to it as it adds to its own map, with the same count and the same clock, so the quota counts exactly what the usage report counts: what a queue took. At midnight UTC the map starts empty.
- At start the server loads the settings and adds today's rows of `usage_daily` to the gate before it takes a request, so a restart does not give a project its quota again. It loses the counts of the last 5 s not yet written, as the usage report does.
- Only the server measures. The project is the one the key authenticated, the event count is what parsing found, and the log size is the decompressed body the server read. No field a client sends, such as an envelope item's `length` or a client report, adds to or takes from a quota.
- The request that crosses a quota is accepted whole: the gate checks before it reads a body, as checking after would cost the parse the refusal is meant to save. A project overshoots by at most the requests in flight when it crossed, each capped at 64 MiB decompressed ([Sizes and counts](#sizes-and-counts)). The next request is refused. A server killed hard loses up to 5 s of counts, which it then takes again; `davy project quota --help` says both.
- Davy's own log forward ([Self-monitoring](#self-monitoring)) asks the gate like any sender. Past the quota of its own project it drops the lines and counts them as `davy_quota`, so a server that logs in a loop cannot fill its own disk either.

### Responses

- An SDK route (`/store/`, `/envelope/`) past a quota answers `429` with `Retry-After` in whole seconds until midnight UTC, at least 1 and at most 300, and `X-Sentry-Rate-Limits` with an entry per refused category after Davy's fixed entry, with the same wait: `86400:transaction;span;replay;profile;profile_chunk:organization, 300:error:project:davy_quota, 300:log_item:project:davy_quota`. The wait stops at 5 min because an SDK that obeys a wait until midnight sends nothing after a person raises the quota; asking again every 5 min costs a refused project one cheap `429` per SDK, and a raised quota takes effect within 5 min. The scope is `project` and the reason `davy_quota`, so an SDK stops sending that category of that project until the time given, and its client report names why. A project past its log quota but not its error quota gets only the `log_item` entry, so its SDK keeps sending errors.
- `/store/` carries one event, so it is refused after the key is checked and before the body is read.
- A request refused before its body was read has its body drained first: read and dropped, up to the route's body limit (20 MiB compressed) and for at most 2 s, without decompressing, paying for or parsing it. A client that writes its body before it reads the answer, as Python's `http.client`, Go and Node `fetch` do, would otherwise see a socket reset instead of the `429` and retry at once. A body that declares more than the limit, sends more, takes longer or waits for `100 Continue` is cut, and the answer says `Connection: close`, so the server reads nothing more of that connection.
- An envelope can hold several signals. Davy reads its items as before and skips an item of a refused signal without parsing its payload. When nothing is left to accept, the answer is `429` as above; when something is, it is `200` with the entries but without `Retry-After`, so the SDK keeps what was taken and stops sending the refused category. Sentry answers an envelope the same way.
- A log route (Logtail, OTLP) carries logs only, so it is refused after the token is checked and before the body is read. OTLP answers `429` with `Retry-After` and a `google.rpc.Status` with code 8, `RESOURCE_EXHAUSTED`, and the message `over the project's daily quota`, in the request's encoding, which the OTLP specification asks of a server that throttles.
- A Vercel drain is the exception. Vercel retries a failed delivery and turns off a drain whose deliveries keep failing, so a `429` until midnight would cost the project its drain, which a person then has to turn on again by hand. A drain past the quota answers `200`, and its records are dropped and counted as `davy_quota`. To count them Davy reads the body, so it reads it only for a project with a drain secret, after the signature matched: a body with a wrong signature is `403` and costs the quota nothing, so a stranger who knows a drain URL cannot fill a project's refusal count. A drain without a secret is refused unread with a `429`, as Logtail is.
- The fixed `Retry-After: 1` of a full queue stays: a `429` that set no `Retry-After` of its own gets it.

### Refusals

- Davy counts the items it refused in `client_reports` with the reason `davy_quota` and the category of the signal, `error` or `log_item`, on the UTC day it refused, and `refused_today` shows that count: items on every route, never requests. A refused `/store/` request counts 1 event. Of an envelope's events only the first counts, as Davy stores only the first ([Sizes and counts](#sizes-and-counts)). A skipped envelope log item counts its records, read from its `items` array without parsing them. A signed Vercel drain counts its records, as its body is read anyway.
- A Logtail, OTLP or unsigned Vercel request is refused before its body is read, so Davy does not know how many records it held, and counting it as 1 item would mix two units under one name. It counts 1 request in `refused_requests` with the reason `davy_quota`, shown apart as `refused_log_requests_today`. The refusals add up in memory and go to the writer with the usage counts every 5 s, in a job of their own, so a refused request never waits for SQLite. A count the writer did not commit goes back for the next flush.
- They are Davy's own counts, so they skip the cap of 100 reason and category pairs per project and day that bounds what SDKs report: an SDK cannot crowd them out. A project deleted in between is skipped.
- A request refused with a `429` because the body budget or a queue was full ([Sizes and counts](#sizes-and-counts)) is not a quota refusal, but a project that is refused this way for hours needs to be seen too. Davy counts it as 1 request in `refused_requests` with the reason `davy_busy`, under the project its key picked, shown as `busy_refused_requests_today`. When the budget runs out before the key is known, as for an envelope whose DSN is in its header line, or for a heartbeat, whose token names a monitor, it counts under project 0, no project, which `davy usage --json` and the MCP tool `usage` show as `busy_refused_requests_today_no_project`, and `davy usage` as a `no project` line when it is not 0. `refused_requests` keeps as many days as `client_reports`.
- The usage page counts them as rate limited, with `davy_rate` and `davy_daily_cap`. Those were accepted first and then refused by the writer, and these never were, so the page shows rate limited between what was accepted and what never was.

### Quota alerts

- A project's quota alerts go to the channels of the project and of all projects, like an issue notice, through the delivery and retries of [Alerts](#alerts). `quota.warn` goes out when a signal's use today reaches the warn percent of its quota, rounded down, and `quota.reached` when it reaches 100%. Each goes out at most once per project, signal, UTC day and quota: a person who raises a reached quota during the day gets `quota.warn` and `quota.reached` again when the new quota's lines are crossed, as the raise is what the first alert asked for. A request that jumps past both sends `quota.reached` alone; so does a flush that holds both.
- The gate notices a crossing as it counts what ingest accepted, when it reads new settings, and at start after it restored today's counts, so lowering a quota under today's use alerts within 5 s. The crossing goes to the writer with the refusals. The writer inserts a row of `quota_alerts(day, project_id, signal, kind, quota)` and queues the notice in the same transaction only when the insert added the row, so a restart, a second flush or a lost reply never sends one twice. `quota.reached` also inserts the warn row. At start the gate reads today's rows back, so it does not hand the writer a crossing it already sent. Rows older than 7 days go in the sweep.
- A webhook body carries a `quota` object: `{"id": 31, "type": "quota.warn", "project": "shop", "at": "2026-11-26T14:02:11Z", "quota": {"signal": "logs", "used": 1600000000, "quota": 2000000000, "percent": 80, "warn_percent": 80, "resets_at": "2026-11-27T00:00:00Z"}}`. `used` and `quota` are error events for `errors` and bytes for `logs`. The Slack line is `[shop] Logs at 80% of the daily quota: 1.6 GB of 2.0 GB`, or `[shop] Logs reached the daily quota of 2.0 GB, refused until midnight UTC`; errors are written as `4000 of 5000 events`. An e-mail has `signal`, `used`, `quota`, `percent`, `warn_percent`, `resets_at` and `at`.
- A DSN holder can drive a project to its quota, so these are bounded by construction: at most four a day per project, two signals and two kinds, which cannot crowd out an operator's monitor alerts.
- `davy doctor` warns for each project at or past a quota today, with today's use, and names the command that raises it. A project near its quota is not a problem for doctor: the warn alert says so. With no quota set, or none reached, the check is ok and says how many projects have one.

### Spike protection

A daily quota stops a project that sends too much in a day, but not one that sends a day's worth in a minute: a loop that throws ten thousand events a second fills the writer queue, and every other project's requests wait or get `429` for a full queue. Spike protection is a token bucket per project and signal on items a second: on by default for error events, off by default for log records.

- **The buckets.** Each project has a bucket for error events and one for log records, never one for both: an error event costs the store writer about 50 times what a log record costs the log path, and log senders send in batches, so a log burst in a shared bucket would refuse the same project's errors and check-ins. Sessions, feedback, check-ins and other envelope items meet no bucket; they are few and cheap, and a refused check-in reads as a missed run. A bucket of rate `rate` holds 10 seconds of it: a project that was quiet may send `10 × rate` items at once, then `rate` a second. It is a generic cell rate algorithm on one number per bucket, the time it is full again, in nanoseconds, so a check is a subtraction and a charge an addition, with no floating point and no timer. A bucket charges what the usage counter counts as accepted, with the same count and clock as the quota, after the request was taken, so a request of 5,000 OTLP records is taken whole and puts the bucket in debt. The debt stops 10 seconds past the refusal line: one request, however large, makes the project wait at most 10 seconds, so `--spike-logs 1` and one request of 2,000 records refuse logs for 10 s, not 33 minutes. The buckets live in the gate next to the quota counts, under the same lock, and are not stored: a restart starts every bucket full, and a changed rate starts its bucket full.
- **The default for error events, 4,000 a second with a burst of 40,000.** Measured on 2026-11-26 on a 12-core development Mac under a load of about 6 from other work, release build, with `bench/load-events.sh`: the store committed 5,683 to 5,866 error events a second over 8 connections, 7,312 over 16 and 7,854 over 32, each answered `200` only after the commit. An error rate of thousands a second is the incident this protects against, not a normal day. 4,000 a second is 70% of the slowest measured commit rate, so one project flooding errors leaves the writer at least 30% for the others. A server on slower hardware can lower it, and a project that really sends more can raise it.
- **No default for log records.** The log route took 116,343 rows a second over 4 connections (`bench/load-logs.sh`), and a heavy normal workload, 300 GB of raw logs a month, is about 290 rows a second on average ([Log storage benchmark](storage-benchmark.md)). Averages say little here: a collector that catches up after 12 minutes away sends 200,000 lines at once, a batch job 50,000 in a second, and with one bucket at the error default both were refused for tens of seconds, errors included. The log path already has its backstops, the body budget and the full-queue `429`, so a log bucket is a person's choice for a project they want to hold back, with `--spike-logs RATE`.
- **Settings.** `davy project quota <slug> --spike RATE` sets the rate of error events and `--spike-logs RATE` that of log records, `off` turns either off and `default` goes back to the default. The columns `spike` and `spike_logs` are `NULL` for the default, `0` for off and the rate otherwise. `--clear` sets both back to the default with the quotas.
- **Where it is checked.** On the same paths as the quota, after the key picked the project. `/store/` and every log route are refused before the body is read, and the body is drained unread, as for a quota. An envelope is read and parsed like an envelope past a quota: the items of a full bucket are dropped and counted, and the rest is taken, so a check-in or a session in the same envelope as a refused error event is kept. An envelope with nothing left is refused.
- **The response.** `429` with `Retry-After` in whole seconds until the bucket takes items again, at least 1, and an entry for the category of the full bucket, `S:error:project:davy_spike` or `S:log_item:project:davy_spike`, after Davy's fixed entry and any quota entries: `86400:transaction;span;replay;profile;profile_chunk:organization, 1:error:project:davy_spike`. The body and OTLP's message say `over the project's spike rate`. An envelope refused for both signals waits for the sooner of the two. A signal past a quota and its bucket at once is refused for the quota, with `Retry-After` until midnight UTC, at most 300 s, as waiting for the bucket would not help. A request that is taken answers `200` with the quota entries only and no spike entry: the bucket is full again within seconds, and an SDK that stops a category for the entry's whole wait would drop more than the bucket refused. A signed Vercel drain is checked against its signature first and answers `200` past a bucket, its records dropped and counted, as for a quota.
- **Refusals.** Counted like quota refusals, with the reason `davy_spike`: items in `client_reports` with their category, `error` or `log_item`, and log requests refused unread in `refused_requests`. The usage page counts them as rate limited.

### Cost and trust

- A request with an unknown key costs the one key lookup it cost before and counts nowhere: the gate runs only after the key picked a project.
- The check is one lock and three map lookups per signal (settings, today's counts, the bucket), the same as counting an accepted request, and asked once per request. A `429` does less than a `200`. On `/store/` and the log routes it drains the body without decompressing, holding or parsing it, and counting it is one more map entry under the same lock. An envelope past a bucket is parsed, as its other items may be taken, but nothing of it is scrubbed, queued or committed. Measured on 2026-11-26 on the machine above under a load of about 40 from other work, release build, `ab -k -c 32 -t 5` with a 38-byte event, server CPU time divided by requests: `/store/` took 14,011 events a second at 128 µs of CPU each, and refused 62,853 a second past a quota at 30 µs and 68,765 past the spike rate at 28 µs; an envelope took 12,678 a second at 132 µs and refused 67,957 past the spike rate at 58 µs. A refusal costs a quarter of an accept, and an envelope refusal less than half.
- `e2e/run.sh` waits for the next UTC day when less than 10 minutes of this one are left, as the quota and its count start again at midnight, then sets a quota of 2 error events on one project of the running release binary and sends events to its `/store/` route until one answers `429`, within a deadline of 30 s as the server reads the setting within 5 s. It asserts that `X-Sentry-Rate-Limits` holds `<Retry-After>:error:project:davy_quota` next to the entry for the categories Davy does not store, that another project still gets `200`, that `davy usage --json` shows the quota and, once the 5 s flush wrote it, at least one `davy_quota` refusal, and that `davy doctor` warns about the project in its `quotas` check.

## Self-monitoring

Davy records its own warnings, errors and crashes in a project of its own, usually `davy`, in its own store. An agent that triages an app over MCP finds the server's problems with the same tools: `search_logs` and `log_patterns` with that project for lines, and `issue_context` for a crash. `project_status` names the project under `davy.project`.

### On by default

Self-reporting is on unless `davy serve --no-self-report` is given. A server that runs as a service is read by an agent, not by a person who tails stderr, and the problems it reports are exactly what that agent needs to explain missing data: a day of logs retention skipped, a commit that failed, a notifier that cannot read its work. The cost is small and bounded, see below, and the project shows in usage, the forecast and `log_patterns` like any project, so it is never hidden disk use. Off by default, nobody would turn it on before the first problem, and then its lines would already be lost.

### Davy's own project

The project is marked, not found by its slug: schema version 22 adds `projects.own`, 1 for Davy's own project and 0 for every project a person made, with a unique index that allows one marked project.

- `davy serve` finds or makes it at start, before the store writer and the log writer start, so no second writer touches SQLite. It is made with 30 days of logs and the default scrub policy plus e-mail and personal data, and an info line says so and names `--no-self-report`.
- It takes the slug `davy`. When a person made a project `davy` before, that project stays theirs with its own settings and data, and Davy's own project takes the first free slug of `davy-self`, `davy-self-2` up to `davy-self-100`; the info line says why. `project_status` and `davy doctor` name the slug.
- A store that cannot be opened or a project that cannot be made is said on stderr, and the server runs without recording itself.
- The project is not counted as a first project or a first error of the setup steps, and the connect page's first log ignores it.

### Lines

Every line Davy printed to stderr goes through `self_report::{info, warn, error}` with a component, such as `log writer`, `retention`, `scrub`, `sweep`, `ui`, `usage flusher`, `checker` or `notifier`. The helper prints `component: message` to stderr as before, then offers the line to the sink. New code uses the helper, and `clippy::print_stderr` stays denied outside it. A failure that never printed, such as a writer job that fails and becomes a 5xx, makes no line.

- A record has source `davy`, service `davy`, the line's level, its message and the attributes `component` and `davy.version`. It is scrubbed with the project's policy like any log.
- Records count in usage under Davy's own project, like logs a client sent. The disk forecast leaves them out, see "Forecast".

### Bounds

- The sink is a channel of 1,024 lines. Offering never waits: when the channel is full the line is dropped and counted, stderr says once that lines are dropped, and the next line queued carries the count in `davy.dropped`. A message is cut at 8 KiB; stderr gets it whole.
- A component and message pattern (`logs::patterns`) is recorded 10 times an hour. The pattern is the first 12 tokens of the message with their variable parts masked, so two causes behind one long prefix share one limit. Further lines are counted, and the next one recorded carries the count in `davy.suppressed`. At most 1,024 patterns are counted at once; past that, once the windows that ended are forgotten, every new pattern shares one overflow window of 10 lines an hour. A count of a window forgotten before a line carried it, and the counts a dropped line carried, are added to the next `davy.dropped`, so no count is lost.
- When the forwarder is gone, such as after a panic of its thread, the first line offered uninstalls the sink and says once on stderr that Davy's own lines are no longer recorded.
- `davy serve` installs the sink before it starts the store and log writers, so what their first pass reports, such as a stray day of logs found by the first retention pass, waits in the channel until the forwarder starts once they run.
- One thread, the forwarder, makes the records and queues them on the log queue in batches of at most 256. It shares the log queue's bounds with ingest, and a full log queue drops Davy's lines rather than wait.

### Crashes

A panic of `davy serve` becomes an issue of Davy's own project: a fatal, unhandled exception of type `panic` with the panic message, the thread and the location as tags, a Rust backtrace and the release `davy@` and the version that panicked. It groups like any error, by its innermost frames of the crate `davy`, which are in-app, so a panic that repeats is one issue with a count.

- The panic hook writes a crash file before the hook that prints the panic runs, synchronously, so the report survives a panic that ends the process, and one in the log writer or the store writer, which could not record it. The file goes to DATA/crashes, made with mode 0700, as a hidden file of mode 0600 that is renamed into place, so a reader never sees half of one.
- The file holds the panic message as the panic made it: it is scrubbed when recorded, not when saved. The file is 0600 in a directory of 0700 that the hook checks is the process user's, and stderr gets the same text from the default hook.
- The forwarder records the crash files when it starts, every 5 s while the process lives, and once more when it stops. It scrubs each one with the project's policy as an event, queues it to the store writer, counts it in usage like an error a client sent, and deletes the file only once the store confirms it has the event. A store that does not take it, whether busy, stopped or failing, leaves the file for the next scan, said once on stderr. A file whose event the store has already, as when a delete failed, is deleted and not counted again.
- Only regular files of the process user named like a crash file are read, opened without following a symbolic link and without waiting, so a FIFO or a link in DATA/crashes cannot stop the scan or the shutdown. Other entries are named once on stderr and left alone. A file that cannot be read or parsed is set aside in DATA/crashes/refused for a person, and deleted once 20 wait there. A scan reads at most 1,024 entries of the directory. A hidden file of a write a kill interrupted is removed once it is a minute old.
- At most 20 crash files wait. A panic past them is printed and not saved, so a panic that repeats cannot fill the disk. A file holds at most 64 KiB, the innermost 100 frames that fit, and a message of 8 KiB; a larger file, another format or a file with more frames is set aside. Each scan records at most 10.
- The backtrace keeps the frames between the code that starts the panic (`core::panicking`, `std::panicking`, `rust_begin_unwind`) and the start of the thread (`__rust_begin_short_backtrace`).
- A Linux release binary carries the line tables of the crate `davy` (`debug = "line-tables-only"` in `Cargo.toml`, with `debug = false` for std and every dependency), compressed with zlib by the linker, so Davy's own frames have a file and a line. They add about 7 MB; the tables of every crate would add 29 MB compressed and 158 MB not. A macOS release binary has none and its frames name the function only: Apple's linker leaves line tables in the object files, which a download does not have, so they would add 4 MB to the binary and give nothing. A build from source with plain `cargo build --release` has them on both.
- `scripts/build-release` remaps the build directory to `.` and the cargo home to `/cargo`, so no path of the build machine is in a released binary or in a crash it records. Every file of the crate `davy` is named from the crate root, such as `src/logs/store.rs`, wherever Davy was built and wherever it runs: the frame of the panic hook in `self_report/crash.rs` shows what comes before `src/`, and that is stripped from every file, as is a leading `./`. A frame is in the app when its function's path starts with `davy::`, or, for a frame inlined without a path, such as `test_panic`, when its file is under `src/`.
- The hook is set by `davy serve` unless `--no-self-report` is given, and chains to the hook before it. It holds the quiet guard and reports its own failure on stderr only; it never panics, as a panic in a panic hook aborts the process. With `--no-self-report`, crash files an earlier run saved wait, and `davy doctor` names them. The hidden command `davy crash --data DATA` saves a test panic the same way, for the end-to-end test, and says on stderr that it is a test.

### Where it shows

- `project_status` carries a `davy` block for any slug: the slug of Davy's own project, its warnings and errors of the last 24 hours (error counts fatal too), the latest 5 crash issues recorded in them with their ID, title, last time, the time recorded and count, and the crash files that wait. A crash counts by the time it was recorded, so a panic saved days before the server came back up still shows; the panic time stays for display. An agent that polls one project sees that the server itself has trouble without asking for it. It reads the log index and the issues index of Davy's own project only, so it costs one count over a day of that project's logs. When it cannot be read, the block is `{"error": "..."}` and the rest of `project_status` answers as usual.
- `davy doctor` has a check `davy`. A crash recorded in the last 24 hours warns, with the latest crash issue and a fix that names `davy context ID --data DATA` and the MCP call `issue_context` with that `issue_id`. A crash file that waits warns, as only a `davy serve` with self-reporting records it. Otherwise the check is ok and counts the errors and warnings, naming `davy logs --data DATA --project SLUG --level warn` with the project's slug. Warnings and errors alone never warn: they are the server's own report and the other checks judge what they mean.
- The MCP instructions and the descriptions of `project_status`, `search_logs` and `issue_context` say where Davy's own problems are, so an agent knows without reading these docs.

### No feedback loop

A failure while recording one of Davy's own lines is never recorded in turn.

- The forwarder and every call into the sink hold a thread-local quiet guard. A line made while it is held goes to stderr only, so a line cannot recurse into the sink, and a failure in the forwarder, such as a store it cannot open or a log queue that is full, is printed and not recorded.
- The log writer prints and does not record a failed write of a batch of Davy's records, and a failed commit that held only Davy's records. Either would write to the index that just failed.
- The usage flusher prints and does not record counts it could not store when the counts held are those of Davy's own project alone.
- What the guards miss is caught by the rate limit: a loop records at most 10 lines an hour.

A sink, forwarder or project that fails never fails a request or the server: lines go to stderr as before. On shutdown the forwarder records what is queued before the log writer stops; lines from then on go to stderr only.

## Savings

Where logs and errors could take less: the patterns with the most bytes and when a search last asked for each, the log retention each project's searches need, and the issues that send most of a project's error events. The `usage` tool and `davy usage` show it as a view of their own.

### Search history

The savings view needs to know what people and agents read. Davy records each log search in `searches(id, at, by, project_id, since, until, level, pattern, query)` and adds it to its day in `search_days(day, project_id, searches, oldest_read_days)`, both added by migration 23:

- `search_logs` and `count_logs` over MCP record `by` `agent`. The logs page of the web UI records `by` `ui` for the first page of a search, once the search holds its permit, so a search turned away as busy is not recorded; the pages before it, the live tail's polls after it and the histogram are not searches of their own. `davy logs` opens the store read-only and records nothing, and neither does `issue_context`.
- A row holds the filters as the search ran them: the project's id or null for every project, the window in unix nanoseconds, the minimum level as a number from 1 (trace) to 6 (fatal), the pattern id as the signed bits of its 64 bits, and the query text, trimmed, or null. `at` is when it ran, in unix milliseconds.
- A search is queued fire and forget: a full writer queue drops it, so reading logs never waits on the writer and a burst of searches cannot hold ingest. The writer stores a search in a transaction of its own, so a search that fails rolls back no ingest. A query text over 1,000 bytes of UTF-8 is recorded as null, with the window and the other filters. The web UI takes a query of up to 1,000 characters, so a query of other scripts can pass it and still be over 1,000 bytes.
- Each insert deletes the rows 10,000 ids or more behind it, a range of the primary key, so the table holds at most the newest 10,000 searches whatever the rate. The sweep deletes searches run more than 90 days ago.
- `search_days` keeps, per UTC day of `at` and per project, or for every project with `project_id` 0 (a search over every project), the number of searches and `oldest_read_days`, the most whole days, rounded up, that a search of that day read back from when it ran. Each search adds one, the count stopping at the largest 64-bit integer, and keeps the larger oldest read. No trim of `searches` removes a day, so a busy day of short reads cannot hide a long read from earlier, and the days the rows cover tell how long Davy has recorded searches. The sweep deletes the days before the 90 days, at most 10,000 rows a sweep.
- The rows say what was asked, not who asked it: a query text an agent sends can hold anything, so the savings view reads it as data, and the table is in the `sql` tool's schema like any other.

### The savings view

`usage` with `view` `savings` and `davy usage --view savings` read the same window as the summary and answer with it alone, not the summary: `window` (`since`, `days`), `savings`, a list of entries, and `note`, which says that `last_queried` is approximate, which reads the view cannot see, and that discarding an issue also stops storing its real events. The summary stays the default view and is unchanged. `top`, 3 by default, from 1 to 20, bounds each kind of entry: at most `top` patterns, `top` shadow rules, `top` retention entries and `top` issues. Over MCP a `top` outside that range is clamped; `davy usage --top` refuses it. Sizes are strings such as `4.1 GB` over MCP and bytes with `davy usage --json`. The entries come in this order, each kind sorted by what it frees:

- `pattern`: the log patterns with the most raw bytes in the window, as `log_patterns` counts them, without the row of patterns too rare to count on their own. Each has `id`, `project` (the project asked for, else null), `service` and `level`, the most frequent, `example`, `bytes` and `per_day` (raw bytes, as `log_days` counts them), `share` of the window's raw bytes, `last_queried` (see [Last queried](#last-queried)) and `try`, `search_logs pattern=<id>`. When the counters behind them are cut short, unread or restarted, `patterns_partial` is true.
  In the view of one project a pattern also gets a share to keep with a drop rule (see [Drop rules](#drop-rules)): `suggest`, `keep 1%; a person can run: davy drop add --pattern <id> --keep 1% --data DATA -- <slug>`, the same as `rule`, the rule in the words `davy drop list` uses, such as `pattern <id> below warn, keep 1%`, and `command`, the command alone, `saves_per_day`, the raw bytes a day the rest would free, `per_day` times the share dropped, rounded down, and `shadow`, the call an agent makes to count the rule first without dropping anything, `drop_rule_shadow project="<slug>" pattern="<id>" keep="1%"`. The share is 1% for a pattern no search asked for and 10% for one a search did, so a pattern someone reads keeps a sample. `saves_per_day` of a pattern divides by the days of the window, as `per_day` does. The three are null when the pattern's lines reach warn, as dropping warnings and errors hides trouble, when a rule of the project names the pattern already, live or shadow, for Davy's own project, and in the view of every project, since a rule is of one project and a pattern's counters do not say which projects sent it. When a live rule of the project already matches the pattern's example, tried at the highest level of its lines and with its most frequent service, `suggest` is `an earlier rule decides these lines: rule <id>` and the others are null: the first live rule that matches decides, so a rule added after it would never see these lines.
- `shadow`: the project's shadow rules (see [Drop rules](#drop-rules)), the most bytes they would have dropped first, with `id`, `project`, `by`, exactly one of `pattern`, `level` and `message`, `service`, `keep_percent`, `created_at`, `expires_at`, `hours`, the hours it has counted, to one decimal, `would_drop`, the records and raw bytes it would have dropped in the window, `saves_per_day`, `rule`, the rule in words, `hides`, `suggest` and `command`. A rule counts from the latest of the start of the window, the time it was made and the start of the first UTC day it counted a line on as a shadow rule, so a rule that was live and is a shadow rule again divides by its days in shadow, not by the days it was live. `saves_per_day` is those bytes over the days counted, and null until the rule has counted a full day: a rate of a few hours says little about a day, so before that the entry gives the hours and the raw counts alone. `suggest` is `make it live; a person can run: davy drop live --data DATA <id>` and `command` that command alone, both null for a rule on Davy's own project or one that can drop warnings and errors (a message rule, a level rule at warn or above, or a pattern rule at any level): `hides` then says what it would hide, such as `every line that holds the text, warnings and errors included`, and is null otherwise. A person can still make such a rule live with `davy drop live`, which asks first. A live rule has no entry: what it dropped is `logs_filtered` in the summary.
- `retention`: the projects that keep logs longer than their searches read (see [Retention fit](#retention-fit)), with `project`, `logs_days`, `oldest_read_days`, `searches`, `history_days`, `saves`, `suggest`, `keep 14 days; a person can run: davy project retention --logs 14 --data DATA -- <slug>`, `command`, the command alone, and `try`, a read-only call that counts the logs the cut would delete: `count_logs project="<slug>" since=90d until=14d`, the slug as a JSON string. The command puts its options first and the slug after `--`, so a slug such as `-x` is not read as an option.
- `issue`: the issues that send most of a project's error events (see [Noisy issues](#noisy-issues)), with `id`, `project`, `title`, `events`, `share`, `suggest`, `fix it, or a person can discard it, which also stops storing its real events: davy issue discard <id> --data DATA`, `command`, the command alone, and `try`, `issue_context <id>`. Anyone with the project's DSN can send 100 events that group into a real issue, so the suggestion says what discarding costs.

Davy's own project (see [Self-monitoring](#self-monitoring)) has `pattern` and `shadow` entries like any project, its shadow entries with `hides` and no command, but no `retention` or `issue` entry and no share to keep: its lines are few, as one kind of line is recorded 10 times an hour, so a shorter retention saves little, and a crash of Davy is to be fixed in Davy, not discarded. A person can still discard one with `davy issue discard`.

No entry changes anything, and no `try` writes; `shadow` adds a rule that drops nothing. Only a person shortens a retention, discards an issue or makes a drop rule live, by running the command shown, as no MCP tool can do any of them: an agent's prompt can carry text someone else wrote, such as an event's message. `suggest` keeps the sentence an agent quotes; `rule` and `command` give the same parts on their own, so the web UI shows the rule in Davy's words and copies the command without parsing a sentence. The slug and the data directory in a command are quoted for a POSIX shell, and the data directory is absolute, so a command works from any directory. Davy has no default data directory, as every command that opens the store takes `--data`, so the command names it, and a person who runs Davy from a deep path sees that path in each command. The text view prints titles, examples and slugs with control characters and bidi marks as U+FFFD, so a log line cannot move the cursor or forge a line, and `davy usage --json` writes C1 controls and bidi marks as `\uXXXX` escapes. It says `1 day` and `1 search` in the singular. With nothing to suggest, `savings` is empty and the text says `nothing to suggest`.

### Last queried

`last_queried` is the time of the newest search that asked for a pattern, in RFC 3339, or null. It is approximate, and the view says so in `note`: Davy does not keep which lines a search returned, so it matches each search's filters against the pattern's example.

- It reads the newest 1,000 searches with a text or a pattern filter, a walk down the primary key that stops once every pattern has a time. A search with neither, which browses a window, asks for no pattern.
- A search asks for a pattern when its project is the project asked for, every project, or the view asks for every project, its minimum level is at most the highest level the pattern's records have, its pattern filter, if any, is the pattern, and its text, if any, finds the example. The text is parsed as `search_logs` parses it, against an index in memory of the examples' messages, at most 20 of them, written per view and dropped after. The window is not compared, as the example has no time.
- So it errs both ways: a search for a word in a line's attributes, or in the part of a line the example does not share, does not count, and a search whose window missed every line of the pattern does. A pattern asked for only by `davy logs`, which records no search, reads as never queried, and so does one asked for only before the newest 1,000 searches.

### Retention fit

Per project, Davy compares the oldest data its searches read with the logs it keeps.

- It reads `search_days`, not `searches`, so the trim of `searches` to 10,000 rows cannot hide a read: a day keeps its count and its oldest read whatever came after.
- A search read back to its `since`: at the time it ran, that is `at - since` rounded up to whole days. `oldest_read_days` is the most of the days of the project and of every project from 30 days back to today, and `searches` their sum. Browsing counts here, since a person who scrolls back a week needs that week.
- `history_days` is the whole days from the first day `search_days` holds to today, that first day left out as its first search may have come late in it, at most 30. Davy suggests nothing under 7, as a week without a search says little.
- With no search of the project or of every project in those 30 days there is no entry: Davy records the searches of the web UI's logs page, `search_logs` and `count_logs`, not `davy logs` or `issue_context`, and drops a search when the writer is busy, so no search may still be a read it did not see. The note says so for every entry.
- The suggestion is the smallest of 7, 14, 30, 60, 90, 180 and 365 days that is at least twice the oldest read, and at least 7 days, and under the project's log retention. With none smaller than the retention, there is no entry.
- `saves` is the raw bytes `log_days` counted for the project on the days the cut frees that retention has not cut yet: log retention keeps a day while it is not before the day N days back, so going from N days to S frees the day N days back up to the day before the day S days back, times how logs pack on this server, the forecast's middle ratio, else the footprint benchmark's. It is an estimate: the project's lines pack like the server's average. An entry that saves nothing is left out.

### Noisy issues

From `issue_hourly`, an issue is noisy when it sent at least 20% of its project's error events in the window, and at least 100 of them, as a share of a handful of events is not noise. `issue_hourly` counts events by their own time, floored to the hour, so a window counts the events that say they happened in it. A discarded issue is left out, as its events are already not stored. `share` is of the project's error events in the window, to two decimals. The suggestion is to fix it, or to discard it (see [Discarding an issue](#discarding-an-issue)).

## Drop rules

A pattern that fills a project's logs and that nobody reads can be dropped before it is stored. A drop rule names lines of one project, and the log writer drops them, or all but a share of them. A rule starts in shadow, where it drops nothing and counts what it would drop, so a person sees what a rule costs before it costs anything. Rules live in `src/drops.rs`.

### What a rule names

A rule names exactly one of:

- `--pattern ID`: the lines of a pattern below warn, 12 hex digits as `log_patterns` and the savings view show it (see [Log patterns](#log-patterns)). `other` is refused, as it counts many kinds of lines. A pattern groups lines by their text, not their level, so the same text logged once as a warning is skipped unless the rule says `--any-level`, which only `davy drop add` takes, never an agent. A pattern rule at any level is another rule than the same one below warn.
- `--level LEVEL`: the lines of that level and every lower one, so `debug` takes trace and debug. A line whose level was missing or not understood is never taken by a level rule, though it counts as Info elsewhere (see [Log levels](#log-levels)).
- `--message TEXT`: the lines whose message holds the text, byte for byte, case and all. With a `*` the text matches the whole message: the part before the first `*` starts it, the part after the last ends it, each part between comes in order, and the start and the end do not overlap, so `ab*b` does not match `ab`.

`--service NAME` narrows a rule to the lines whose service is that name exactly; `*` is no wildcard in it. Two rules of a project never match the same way: adding one equal to a rule the project has answers that rule, with `created` false, when that rule is in the state asked. When it is not, the add is refused with the command that turns the rule, `davy drop live ID` or `davy drop shadow ID`, so an agent's shadow rule never answers with a live rule and a person's live rule never answers with a shadow one. A person who adds a shadow rule equal to an agent's shadow rule takes it over: the rule becomes the person's, never expires, and the answer says so with `taken_over` true.

A message text and a service are refused when they are:

- empty, or over 256 bytes;
- only `*`: a message of only `*` matches every line, so the answer says to use `--level` or name the text, and a service of only `*` says to leave `--service` out;
- holding a character a person cannot see or tell from a space: a control character, a format character (Unicode category Cf, such as U+200B or a bidi mark), U+2028, U+2029 or U+00A0. The answer names it as `U+XXXX`.

`--keep N%` takes digits only, so `+5` and `5 %` are refused.

`--keep N%`, from 1 to 99, keeps that share of the lines the rule matches; without it the rule drops them all. Keeping is exact, not random: each rule adds N to a counter per line it matches and keeps the line each time the counter reaches 100, starting one step short of it. So the first line is kept, then exactly N of every 100 after it: 1% keeps lines 0, 100, ... 900 of 1,000 and 33% keeps 330. The counter lives in the log writer and starts over when the rule changes or the server restarts, so a line is kept at each start.

### In what order

Per project, the log writer tries the live rules in their order and the first that matches decides: it drops the line, or keeps it when its share says so, and no later live rule sees it. A line no live rule matched then goes to every shadow rule, each counting on its own what it would drop. So a shadow rule counts what it would add to the live rules in force, not what they already take.

A rule is added after the project's others. Making a rule live puts it after the project's live rules.

### Shadow and live

- A live rule drops a line before it is stored: the line is in no index, no pattern counter and not in `logs_stored`. Ingest still accepted it, so it counts in `logs` like any line a client sent, and it counts as `logs_filtered`, see below.
- A shadow rule drops nothing. Per UTC day it counts the lines it would drop and their raw bytes, as `logs_stored` counts raw bytes. The savings view shows each with what it would save a day and the command that makes it live, see [The savings view](#the-savings-view).
- `drop_daily(day, rule_id, live, project_id, records, bytes)` holds both counts per UTC day of receipt (`observed_at`), rule and state. The log writer counts in memory and the usage flusher writes the counts with the logs stored every 5 s, in the same transaction, so a crash loses the same last seconds of both. A rule a person removes keeps its counts until they are pruned, while an agent's shadow rule that the agent removes takes its counts with it, as they only counted a proposal. A removed rule's id is never used again, so a new rule never inherits them. The sweep deletes the days before the 400 days that end today, at most 10,000 rows a sweep, as it prunes `usage_daily`. Per day a project has at most a row per rule and state, and at most 50 rules at once.
- `usage` and `davy usage` show `logs_filtered` per project: the records and raw bytes live rules dropped on the UTC days of the window. Shadow counts are not in it, as nothing was dropped.

### Who may do what

The risk of a rule is a rule that drops what someone needs. So only a person makes a rule that drops, at the CLI, or by unlocking destructive changes for one agent token of level `full`, whose agent may then call `drop_rule_live` (see [Agent tokens](#agent-tokens)).

- A person adds, lists, toggles and removes rules with `davy drop`, the only way to write a live rule or make a rule live:
  - `davy drop add (--pattern ID [--any-level] | --level LEVEL | --message TEXT) [--service NAME] [--keep N%] [--shadow] --data DATA SLUG` adds a rule, live unless `--shadow` is given, and prints it as JSON.
  - `davy drop list [--json] --data DATA [SLUG]` lists the rules in the order the log writer tries them, with what each dropped or would drop in the last 7 days, today included. `--json` gives raw bytes.
  - `davy drop live ID [--yes] --data DATA` makes a rule live, `davy drop shadow ID --data DATA` makes it a shadow rule again, `davy drop remove ID --data DATA` removes it. A person holds a rule once they toggled it, and it never expires.
  - Before a shadow rule goes live, `davy drop live` prints on stderr the rule as the list words it, the lines and raw bytes it would have dropped in the last 7 days, and `it can drop warnings and errors too` when it can (a message rule, a level rule at warn or above, or a pattern rule at any level), then asks `make it live, so these lines are dropped from now on? [y/N]`. Only `y` or `yes` goes on; any other answer leaves it a shadow rule and fails. `--yes` asks nothing. Without a terminal on stdin and without `--yes` it refuses and says to add `--yes`, so a script never makes a rule live by accident. The command a listing prints carries no `--yes`, so a person who pastes it is asked.
  - `live` and `shadow` refuse an agent's rule that expired, though the sweep has not deleted it yet: it answers `no rule ID, or it expired`.
  - `list` opens the store read-only, and `live`, `shadow` and `remove` open it without creating or upgrading it. A data directory with no store, or a store of an older schema, is an error that says to run `davy serve --data DATA` first or once; a store of a newer schema is refused as elsewhere. `add` opens the store as `davy project` does.
- An agent may only add and remove shadow rules, over MCP, at most 10 of its own per project at once, and each expires 14 days after it was made. It removes only its own shadow rules. A rule that expired leaves its room at once and the sweep deletes it.
  - `drop_rules` lists the rules of a project or of all, as `davy drop list --json` does with sizes as text, each with `any_level`, true for a pattern rule that takes every level.
  - `drop_rule_shadow` adds a shadow rule, with the fields `davy drop add` takes but `--any-level`, and no other: a `live` or `any_level` field is refused as unknown. It answers the rule with `make_live`, `a person can run: davy drop live --data DATA ID`.
  - `drop_rule_remove` removes one of the agent's shadow rules, and the counts it made with it.
  - Each answer carries `note`: a shadow rule drops nothing and counts from within 5 s, only a person makes it live, and an agent holds at most 10 per project, each for 14 days.
  - An agent cannot stage a rule on Davy's own project (see [Self-monitoring](#self-monitoring)), whose warnings and errors `davy doctor` and `project_status` read, nor a level rule at warn or above, which names warnings and errors. Each is refused with an error that says why and that a person can add it with `davy drop add`.
  - The tools write through the store writer as `Job::Drop`, whose change is a shadow rule to add, one to remove, or one to make live. The last carries `access::Unlocked`, which only a token a person unlocked yields, so the guarantee below is in the type, not in a check of each tool.
- No route or ingest path writes a live rule or turns a rule live, and no MCP tool does for a token a person did not unlock: `drops::add` refuses a live rule from an agent, and the one agent path that calls `drops::set_live` needs the unlock. A shadow rule's listing names the command a person runs to make it live, `davy drop live --data DATA ID` with the absolute data directory quoted for a POSIX shell, so it works from any directory. An agent's prompt can carry text someone else wrote, so it can propose a rule and count it, never enforce it.
- A list words each rule as `pattern 0000000abcde below warn` or `at any level`, `level debug and below`, `message contains "ok"` or `message like "ok*"`, then `service "api"` and `drop all` or `keep 5%`. A message and a service are quoted, with `"` and `\` escaped by a `\`, so a text cannot close its quotes and read as more of the rule. The project, a message and a service are printed with control characters and bidi marks as U+FFFD, so a rule's text cannot move the cursor or forge a line. The JSON `davy drop` prints and every MCP answer are written through `text::json`, which writes DEL, C1 controls and those marks as `\u` escapes.

### Bounds and cost

A project holds at most 50 rules, live and shadow together, so the log writer tries at most 50 on a line. A project without rules costs a hash lookup per line. The server reads the rules at start, before the first line, and every 5 s after, and compiles them only when they changed: a message becomes a `memchr` finder, or a glob of finders, once per change, not per line. A change made with `davy drop` reaches the log writer within 5 s, at its next batch. A failed read keeps the rules in force and is reported as a warning of Davy's own project.

A pattern rule needs the line's pattern. It is worked out once per line, only when a pattern rule is tried, and the pattern counters take it from there instead of masking the line again, so a pattern rule costs nothing more on a line that is stored.

Measured on an Apple M2 Max with load average 3.3 to 8, with the commands below, which print these numbers:

- The judge alone, `cargo run --release --example drop_judge`: `Judge::keep` over lines made before the clock starts, fastest of 7 rounds, 20,000 access lines of about 90 bytes, or 1,000 lines of 64 KiB, the most a line holds. Rules that never match are tried in full on every line, the most a rule costs on a line that is stored.

  | case | ns per line (measured) |
  | --- | --- |
  | no rule | 1.9 |
  | 10 message rules, none match | 70.7 |
  | 50 message rules, none match | 327.4 |
  | 50 globs, none match | 222.9 |
  | 1 message rule that matches, keep 1% | 57.1 |
  | 1 glob that matches, drop all | 48.2 |
  | 1 pattern rule, drops a quarter | 923.4 |
  | no rule, 64 KiB messages | 2.0 |
  | 10 message rules, none match, 64 KiB messages | 18,719.9 |
  | 50 message rules, none match, 64 KiB messages | 146,084.7 |
  | 50 message rules, 64 KiB of near misses | 92,617.8 |
  | 50 globs, 64 KiB of near misses | 252.3 |

  A message rule reads the whole message, so the cost grows with its length: 50 rules on a 64 KiB message take about 0.15 ms. That is the most a line can cost, so messages are not cut short before a rule reads them. The pattern rule's time is mostly working out the line's pattern, which a stored line needs anyway.
- End to end, `cargo build --release --example fill_logs` and then `target/release/examples/fill_logs DATA 300000 24 4 RULES` (300,000 records of the made-up API over 24 hours in 4 projects, made before the clock starts and handed to `LogWriter` without a copy, wall time including the last commit), with `RULES` shadow rules per project whose message texts no line holds, so every rule is tried on every line and none matches. Five rounds, rotating which run went first:

  | | median (measured) | fastest |
  | --- | --- | --- |
  | 0 rules | 9.65 µs per record | 9.54 µs |
  | 10 rules per project | 9.64 µs per record | 9.47 µs |
  | 50 rules per project | 9.57 µs per record | 9.49 µs |

  The rounds of one run differ by up to 0.5 µs, more than the 0.33 µs 50 rules add by the judge's numbers, so a whole run cannot tell the rules apart.

### Davy's own project

Rules apply to Davy's own project (see [Self-monitoring](#self-monitoring)) like to any project, as its lines go through the log writer.

- A shadow rule hides nothing: Davy's warnings and errors are stored and shown in `project_status` and `davy doctor` as before, and the rule counts them.
- A live rule a person made drops Davy's own lines it names before they are stored, warnings and errors too when it names them (a level rule at warn or above, a message rule, a pattern rule at any level): they are then missing from `search_logs`, `log_patterns`, the `davy` block of `project_status` and the counts of `davy doctor`, and counted as `logs_filtered`. They still go to stderr. A rule can only take what it names, so a rule of `--level debug` takes none of Davy's lines, which are info and above.
- Crashes are issues, not log lines, so no rule drops one.

### Migration 24

Schema version 24 adds both tables in one entry, so a branch that takes the same number renumbers one line:

- `drop_rules(id INTEGER PRIMARY KEY AUTOINCREMENT, project_id, position, live, by, pattern, level, message, service, keep_percent, any_level, created_at, changed_at, expires_at)`, with checks that `live` is 0 or 1, `by` is `person` or `agent`, `keep_percent` is from 0 to 99, exactly one of `pattern`, `level` and `message` is set, and `any_level` is 0 or 1, default 0, and 1 only on a pattern rule, and a unique index on what a rule matches, `any_level` included, so two rules of a project never match the same way. `pattern` holds the signed bits of the id, `level` the level from 1 (trace) to 6 (fatal), and times are unix milliseconds.
- `drop_daily(day, rule_id, live, project_id, records, bytes)`, `WITHOUT ROWID`, keyed by day, rule and state.

### Tested end to end

- `e2e/drops.mjs`, run by `e2e/run.sh` against the running server, adds two live rules with `davy drop add` (debug and below, and half of the lines holding "health check") and sees a rule naming both a level and a message refused. Over MCP it sees `drop_rule_shadow` refuse a `live` field, adds a shadow rule on "poll tick" whose `make_live` names `davy drop live`, and sees `drop_rule_remove` refuse a person's rule, and `drop_rule_shadow` refuse a rule on Davy's own project and a level rule of warn. After the 5 s reload it posts 19 Logtail lines and checks the exact counts in `davy drop list` (4 and 2 dropped, 10 would drop), the 13 lines `davy logs` finds, `logs_filtered` of 6 in `davy usage`, and the shadow entry of the savings view: sizes as strings over MCP, bytes from the CLI, no `saves_per_day` in its first hour, and, as a message rule can drop warnings, no command but `hides`. The info pattern of the poll lines gets keep 1% with its `davy drop add` command, the warn line no suggestion. It then runs the command the listing prints with `sh` and no terminal: it prints the rule and what it would have dropped, refuses and says to add `--yes`, and the rule stays a shadow rule. With `--yes` the rule is live and the person's, and the agent can no longer remove it. A person who adds the same shadow rule as an agent takes it over, with `taken_over` true and no expiry.

## Cold tier

Agents search the last days most, and want old logs kept long and still searchable. Day indexes are fast but take 0.55× to 1.03× of the raw bytes. So a closed day older than the hot days turns into cold files: compressed, read by the same queries, slower to search, and 0.05× to 0.16× the size of the index. Recent days stay as they are. The format is ready for object storage (9b): a file is immutable, says what it is and its version, and has a footer from which a reader fetches one block by its byte range.

### Files

- `data/logs/cold/YYYY-MM-DD/N/<project>.dlc` holds a project's logs of the day, with its pattern counters next to it in `patterns-<project>.json`. `N` is the segment, from 1 up: logs that arrive for a day already cold become the next segment.
- A file is a head (`DAVYCOLD`, version), blocks, an optional zstd dictionary, a footer and a trailer (`DAVYCOLD`, version, footer length). Every number is little endian. `src/logs/cold_format.rs` has the layout.
- A block is a 25-byte header and a zstd frame of about 1 MiB of rows, stored column by column: timestamps as deltas, levels, sources, pattern ids, then the strings. The header holds the rows, the size decompressed, the oldest and newest timestamp and a bit per level, so a search skips a block by its header and a count takes whole blocks from it.
- The footer repeats each header with the block's offset, length and checksum (64 bits of BLAKE3), and ends with a checksum of itself. A file is written once, synced with its directory, and never changed.
- zstd level 9, 1 MiB blocks and no dictionary, chosen by `examples/cold_bench.rs` on the six real corpora and the synthetic rows of the footprint benchmark (`docs/storage-benchmark.md`, "Cold tier"). Cold files take 0.05× to 0.12× of the raw bytes, 0.05× to 0.16× of the day index. Level 19 saved up to 17% more and converted 7 to 21 times slower. A dictionary trained per file never paid for the bytes it takes.

### When a day turns cold

- `davy serve --hot-days N`, 1 to 36,500, or `off` to keep every day hot. The default is 7. An agent debugging a problem asks about the last hours and days, so a week keeps those searches at the speed of the day indexes, and a search older than a week can wait a few seconds. A week of hot days also leaves the forecast the 7 closed days it reads to measure how the server's own logs pack. A shorter default would save little: at a cold ratio of 0.16 each hot day costs about 0.84 of a day's index more than a cold one, so with 30 days kept, 7 hot days take 1.8 times the disk of 1 (11.7 day indexes against 6.6). `davy usage` and `davy doctor` take `--hot-days` too, as they cannot read the server's flags.
- A day older than every retention is deleted by retention, never converted. A day that retention would still cut, for a project with a shorter retention, is cut first and then converted.
- The log writer starts one conversion at a time, the oldest day due first, on a thread of its own, so ingest goes on. Measured with `examples/cold_ingest.rs` on an M2 Max at background priority, ingest ran at 55.2k rows/s while a day of 1M logs turned cold and at 56.2k rows/s with no conversion, 0.98 of it. The conversion took under 19.1 s.
- The steps, each of which a crash may end:
  1. The writer commits and closes the day, writes its pattern counters and seals it for segment N: `cold-seal` in the hot day holds N.
  2. The converter writes segment N into `cold/YYYY-MM-DD/.N.tmp`, syncing each file and the directory.
  3. The writer takes the retention lock, checks that the seal still holds and renames `.N.tmp` to `N`. From this rename a reader takes segment N and leaves the hot day out.
  4. The writer renames the hot day into `.trash` and removes it there.
- Exactly once, through a crash. A reader takes a day's hot index unless its seal names a segment that exists, and every segment that exists. So before step 3 the logs are read from the hot day only, and from step 3 on from segment N only; the rename is atomic. At start the writer, under the retention lock, finishes a sealed day whose segment exists (step 4), takes the seal off one whose segment does not, and removes every `.N.tmp`. A recovery that fails, say on the lock, is tried again on each tick, and no day turns cold before it succeeds; a day whose step 4 fails again is tried again as below, a day whose seal cannot be taken off is reported and left out until the server restarts, and the other days go on (`a_failed_recovery_is_tried_on_each_tick_and_no_day_turns_cold_before`, `a_day_that_fails_recovery_does_not_hold_back_the_others`). `a_crash_after_any_step_loses_and_doubles_nothing` stops a conversion after each of the four steps, starts a writer and checks that the same logs, counts and pattern totals come back, once each.
- Late logs. A log for a day that is cold opens a new hot index for that day, searched with its segments, which turns cold as segment N+1 once it is closed and past the hot days (`a_log_for_a_cold_day_is_kept_hot_and_turns_cold_as_the_next_segment`). A log for a day being converted takes the seal off: the converter stops, step 3 drops what it wrote and the day stays hot until the next try (`a_log_for_a_day_turning_cold_keeps_it_hot`). The writer is the one thread that writes logs and renames segments, so a write cannot fall between the check of the seal and the rename. Retention deleting from a sealed day takes the seal off the same way. A seal that names a segment that exists has step 4 done first, so the log goes into a new hot index and the next segment; a seal with no job and no segment, left by a failure, is only taken off (`a_log_for_a_day_with_a_stale_seal_and_no_job_takes_the_seal_off`).
- A day whose conversion fails before the rename stays hot, is reported as a warning on stderr and in Davy's own project, and is not tried again until the server restarts (`a_conversion_that_fails_leaves_the_day_hot_until_the_next_start`). So is a day whose pattern counters cannot be saved, before it is sealed. A retention lock still held when its wait ends at step 3 is no fault of the day: what the converter wrote is dropped, the seal is taken off and the day is tried again a minute later (`a_lock_held_at_the_rename_leaves_the_day_hot_for_a_later_try`). A failure after the rename, syncing the directories or trashing the hot day, keeps the seal: the segment is read and the hot day is not. Step 4 is then done again by the next log for the day, the next start, or the writer once a minute until it works, with one warning each time it fails. No other day turns cold before, as what stopped step 4 would stop it for the next day too, and each such day holds its logs twice (`a_failure_after_the_rename_keeps_the_seal_and_the_next_tick_finishes`, `a_failure_of_step_4_that_stays_is_tried_each_minute_until_it_works`, `a_late_log_for_a_day_whose_step_4_failed_is_taken_once_the_fault_is_gone`).
- Pattern counters move with the day, so `log_patterns` and the savings view read cold days as before.
- Conversion respects the retention lock (`retention::Lock`) and runs only in the server, which holds the data directory's lock.

### Search over cold days

- `search_logs`, `count_logs`, the histogram, `davy logs` and the web UI read hot and cold days through one function, `search_for`, and give the same rows and counts for the same logs. Hot and cold rows go through the same `Row` and `to_hit`.
- Time, project, level and pattern are compared column by column. Text goes through Tantivy itself: rows that may match are indexed into an index in memory with the schema of their day, and the query runs there as the day would run it. So a cold row matches exactly what it matched hot, for every query the language has. Indexing is the slow part, so first a row must hold every word the query cannot match without, lowercased as Tantivy lowercases it; for an `OR` the clause's rarest-looking list counts, the one whose shortest word is longest. `the_same_logs_hot_and_cold_match_every_query_the_same` writes the same 880 logs hot and cold and runs 202 queries against both, comparing hits and histograms: words in any case and in Unicode, phrases, `AND`, `OR`, `NOT`, `+` and `-`, groups, boosts, phrase prefixes, `*`, fields, attribute paths, ranges, `IN` sets, trace ids, each with a project, a level, a limit of 1 and 7, and windows, patterns and later pages. A query the day refuses, a range or `*` over an attribute path, is refused cold too.
- Newest first. Days are read newest first and blocks newest first by their headers, and a search stops once no block left can hold a log newer than those it has. A search the hot days fill opens no cold file (`a_search_the_hot_days_fill_opens_no_cold_file`).
- A time budget of 5 s for the cold part (`cold::BUDGET`). A text query whose words most rows hold reads about 30k to 90k rows a second (benchmark), so a large cold window could take minutes. When the budget runs out the search returns what it found and `unscanned_to`: logs from the start of the window to that time may be missing. The logs it found at or before that time are dropped, as blocks not read may hold newer ones, so the answer holds every log from its newest down to `unscanned_to`, none missing in between. 5 s keeps an agent's tool call well under the timeouts MCP clients use, and covers a full scan of 10M rows or more, or a selective text query over a few million.
- A count takes a block from its header when the block lies inside the window and holds no level below the one asked for, with no text or pattern to match, so such a count decompresses only the blocks at the window's edges. It checks the block's checksum first, so a count and a search leave out the same blocks; reading the bytes counts against the budget like a search does.
- A block or file that fails its check is left out and counted, never a panic and never a silent wrong count: the note says how many rows of how many blocks and files, and `davy doctor` names them. A file must say it is of the day and the project it lies under, with its times and every block's within that UTC day, so a file copied from another day or to another project's name is left out, not read twice or as another project's (`a_file_copied_from_another_day_is_left_out_and_counted`, `a_file_under_another_projects_name_is_left_out_and_counted`).
- What the caller sees. `search_logs` returns the hits as before, and when it read cold days a second text, `{"cold": {cold_from, cold_to, scan_ms, rows_read, unscanned_to?, corrupt_*?}}`. `count_logs` adds the same object under `cold`. `davy logs` prints the rows on stdout and one line about the cold part on stderr, and with `--count --json` the `cold` object. `issue_context` adds the note as `logs_cold` and `project_status` as `last_log_cold`. In the web UI the logs page, its histogram and an issue's logs get `cut` with the same facts and say where a scan stopped and how many blocks and files were left out. The MCP server keeps its sixteen tools.
- A reader lists a day's parts, reads them, and lists them again. A day that turned cold or lost a segment to retention in between makes the pass start over, so no log is read twice or missed.
- `e2e/cold.mjs`, run by `e2e/run.sh`, sends 60 logs for a day three days back and 3 for now through the Logtail route of the release binary, and records what `davy logs`, `davy logs --count` and four `search_logs` and `count_logs` calls answer over a fixed window. It restarts the server with `--hot-days 1`, polls with a deadline of 60 s until the day's index is gone and its first segment is there, and asserts the same rows and counts, a cold note on each answer that names the whole day and no budget or failed block, the stderr line of `davy logs`, and a search for the 3 newest logs that reads no cold file. The suite's last check, that no file is readable by other users, covers the cold files too.

### Retention, usage and doctor

- Retention per project applies to cold days: a project's file and counters go when the day outlives its retention, the whole day when no file is left, renamed into `.trash` first, under the retention lock, with the counts marked as for hot days. A hot day emptied while segments of the day remain marks only what its cut took, as the other projects' logs are still there. A day turning cold is left to the next pass.
- The disk walk splits logs into hot (`logs_hot`) and cold (`logs_cold`); `logs` is their sum. `davy usage` prints the cold size in the disk line. The JSON is the same from the CLI, MCP and the web UI. Only finished segments count as cold: a segment being written, `.N.tmp`, whose logs its hot day still holds, and a day directory with no segment count as other.
- The forecast keeps today and the hot closed days at the growth, and the other closed days at the growth times the cold ratio: `growth × (min(N, H) + 1) + growth × r × (N − min(N, H))` for N days kept and H hot. `r` is the server's own once it holds days of both tiers: the median disk per raw byte of its cold days over that of its hot closed days, each whole and uncut. Until then it is 0.2, the largest cold-to-hot ratio the benchmark measured, 0.159, with a margin. `basis.logs` gives `hot_days`, `cold_ratio` and `own_cold_ratio`. A what-if that lowers retention frees cold days too.
- `davy doctor` has a check `cold`: what hot and cold logs take, the cold days, blocks and files, every block's checksum, read without decompressing, and that each file is of the day and project it lies under. A block or file that fails warns, names at most 5, and says to restore it from a backup or delete it.

### Security

- Only Davy's data directory is read: only real directories and regular files with the canonical names, never a symlink, as in the hot root.
- Every length in a file is checked against the file and against caps before it is used: a block decompresses into at most the size its header claims and never more than 16 MiB, a block holds at most 262,144 rows, a footer at most 16 MiB, a dictionary at most 1 MiB. A crafted or cut file fails with an error in bounded memory.
- A block's rows must lie within the times its header gives and be of exactly the levels it gives, with info's bit for a block of no known level, or the block fails, so a count or a skip by header never disagrees with a read (`a_block_with_a_row_outside_its_header_is_refused`, `a_block_whose_rows_are_of_other_levels_than_its_header_gives_is_refused`).
- `fuzz/fuzz_targets/cold_file.rs` reads arbitrary bytes as a cold file: the footer, then every block checked, decompressed and matched against a query.

### Left for 9b

- Segments in object storage, fetched by byte range with the footer cached locally, and the same checks on what comes back.
- Compaction of a day's segments into one, for days with many late logs.
- Paging past the budget in the web UI: a time cursor at `unscanned_to` can repeat or skip logs where blocks overlap in time, so the page only says where it stopped.

## Agent tokens

An agent that reads errors and logs needs none of the rights of one that changes retention, and an agent's prompt can carry text someone else wrote. So each agent gets a token of its own with a level, the autonomy dial: what it may do without asking a person. The admin token keeps working as before.

### Tokens

- `davy token add NAME --level read|triage|full --data DATA` adds a token and prints, once, `{"token": ..., "secret": "davy_agent_..."}`. The secret is `davy_agent_` and 43 characters of base64url, 256 random bits; the prefix lets a person and a secret scanner tell it from the admin token. Only its blake3 hash is stored, so a copy of the database holds no secret.
- A name is 1 to 64 of `a-z`, `0-9`, `.`, `_` and `-`, starting with a letter or a digit, so it reads the same in a shell, a log line and the web UI. A taken name is refused, also one revoked less than 180 days ago, so the ledger never names two tokens alike. At most 50 tokens are live at once.
- `davy token list [--json] --data DATA` lists every token, live ones first, each group newest first: name, level, whether destructive changes are unlocked, the last call and the revocation. Never a secret or a hash. It opens the store read-only.
- `davy token level NAME LEVEL`, `davy token unlock NAME`, `davy token lock NAME` and `davy token revoke NAME`, each with `--data DATA`, change a token and print it as JSON. They open the store without creating or upgrading it, as `davy drop live` does. A name no live token has is refused with `no token named "NAME", or it was revoked`.
- A revoked token's next call answers `401`. Its row stays 180 days after the revocation, so the ledger names it, then the sweep deletes it.
- `used_at` is written at most once a minute per token, through the writer, and skipped when its queue is full, so a busy agent costs one write a minute, not one per call.
- An agent token goes where the admin token went: `DAVY_TOKEN` in the environment the agent's MCP client starts from, which the config `davy mcp-config` prints reads, or the bearer token of its MCP config. Never in the environment of `davy serve`: the server refuses a `DAVY_TOKEN`, `DAVY_TOKEN_FILE` or `DATA/token` that starts with `davy_agent_` and says where the token goes, so an agent's secret given to the wrong process stops the start instead of becoming the admin token. `davy mcp-config` itself is unchanged: `--show-token` prints the admin token, and a person who gives an agent its own token sets `DAVY_TOKEN` to it.
- Migration 26 adds `agent_tokens(id, name, hash, level, unlocked_at, unlocked_by, created_at, used_at, revoked_at)`, with checks that `level` is `read`, `triage` or `full` and that only a `full` token is unlocked. The read-only `sql` tool reads `agent_tokens.hash` as NULL, like the other credential columns.

### Levels

Each level holds every right of the ones below it.

- `read`: every tool that only reads.
- `triage`: also the changes a person undoes in a click: resolve an issue, send feedback, stage or remove a shadow drop rule, which drops nothing.
- `full`: also configuration that loses nothing: keep data longer.

A destructive change deletes data or stops keeping it: a shorter retention, a return to a lower server default, and a live drop rule. No level makes one. A person unlocks destructive changes for one token of level `full`, on the Tokens page of the web UI or with `davy token unlock`, and the web UI shows the token as unlocked, by whom and since when. Lowering a token below `full` or revoking it locks it again. Only a person can unlock: no MCP tool changes a token.

The admin token holds `full` and is never unlocked. It sits in every agent config made before agent tokens, so it keeps what it could do before and nothing more: a destructive change stays with a person at the CLI or in the web UI, or with an agent token a person unlocked.

### Classification

`access::need` in src/access.rs is the one table of what each tool needs, and mirrors this one. A test lists the router's tools and fails when a tool has no row.

| Tool | Needs | Why |
| --- | --- | --- |
| `schema` | read | reads |
| `sql` | read | one read-only statement |
| `search_logs` | read | reads; the search is recorded for the savings view |
| `count_logs` | read | reads; recorded like `search_logs` |
| `log_patterns` | read | reads |
| `connect_project` | read | reads an existing project; new projects need the CLI |
| `project_status` | read | reads |
| `usage` | read | reads |
| `drop_rules` | read | reads |
| `release_health` | read | reads |
| `issue_context` | read | reads |
| `ledger` | read | reads what was changed, no secret and no payload; see [Ledger](#ledger) |
| `mark_fixed` | triage | a person reopens the issue in a click, and a regression reopens it |
| `submit_feedback` | triage | stores a note about Davy itself |
| `drop_rule_shadow` | triage | counts, drops nothing, expires in 14 days |
| `drop_rule_remove` | triage | removes the agent's own shadow rule |
| `set_retention` | full, or full unlocked when it would delete | longer loses nothing; shorter deletes for good |
| `drop_rule_live` | full unlocked | a live rule drops lines before they are stored |

`set_retention` needs `full` with destructive changes unlocked when the change would delete data or lose a floor a person set: a setting below the days in effect, or `"default"` where the project keeps its own days. The server asks the stored settings before the call. Arguments the tool would refuse anyway keep the plain need, so the tool says what is wrong with them. The writer checks the same again when it applies the change, so two calls at once cannot slip past, and a cut that does not carry the unlock is refused there too.

### Enforcement

- `/mcp` admits a request whose bearer token is the admin token or a live agent token, and answers `401` with `WWW-Authenticate: Bearer` and the same text otherwise, so the answer never says which of the two a wrong token missed. An agent token is found by its hash in one indexed read on a blocking thread, so a slow disk holds up no other request, then the caller, its name and grant, rides in the request.
- `tools/list` lists only the tools the caller's grant reaches, with `cacheScope: private` where the protocol has cache hints, as the list depends on the token.
- A call above the grant is a tool error, not a protocol error, so the agent reads it: `TOOL needs level NEED, and the token "NAME" holds HELD. A person changes a token's level, and unlocks destructive changes, on the Tokens page of the web UI or with davy token.` A refused `set_retention` starts with why the change would delete, as before. For the admin token the last sentence says that it never makes a destructive change, and who does.
- A tool the table has no row for is `tool not found`, whether or not the router has it.
- A call that the grant reaches and that is destructive gets the proof `access::Unlocked`, which only `Caller::unlocked` makes, for an agent token a person unlocked, and which names that token. `set_retention` passes it to the writer as `By::Unlocked`, and `drop_rule_live` cannot be called without it: its writer job carries it. So no path makes a destructive change without passing the one check.
- The proof is read at the start of the call, and the writer may run the change later, behind other jobs. So the writer reads the token's unlock again, in the transaction that makes the change, and refuses the change when a person locked or revoked the token meanwhile: `the token was locked or revoked while the call waited for the writer, so the change was not made`, and who unlocks it again.
- `set_retention` with the proof refuses to shorten the retention of Davy's own project, as `drop_rule_live` refuses a rule on it: `davy doctor` and `project_status` read its warnings and errors. A person still can, with `davy project retention`.
- `drop_rule_live` takes `project` and `rule_id` and makes a shadow rule of the project live, as `davy drop live --yes` does: the rule becomes a person's and never expires, as the person who unlocked the token answers for it. It refuses a rule on Davy's own project and a level rule at warn or above, as `drop_rule_shadow` does, so an unlocked agent cannot hide Davy's own warnings, and a rule that is live already, unknown or expired.

### Not in this feature

- Tokens limited to some projects. A token reaches every project, as the admin token does; see "Tenant scope in agent tools".
- Expiry dates. A person revokes a token.
- Rights beyond the MCP tools: an agent token opens no other route, such as the scrub settings or source map upload.

## Ledger

An agent with a token makes changes nobody watched it make. The ledger is the record a person reads afterwards: what changed through Davy, who asked, from which MCP client, and how it ended. It is also the record an agent reads before it acts again, so it does not undo a person's choice.

### A row

Each row has `id`, `at`, `actor_kind` and `actor`, `client`, `action`, `project`, `issue`, `target`, `outcome`, `summary` and `repeats`.

- `actor_kind` is `admin`, `token`, `person` or `cli`. `actor` is the token's name, the person's e-mail, `admin token` or `davy CLI`. A space is in no token name and no e-mail, so the last two never name someone else.
- `client` is the name the MCP client gave when it connected, such as `claude-code`, null for a person and the CLI.
- `action` is the MCP tool, or the same change made another way: `issue_status`, `set_retention`, `drop_rule_live`, `drop_rule_add`, `drop_rule_remove`, `token_add`, `token_level`, `token_unlock`, `token_lock` and `token_revoke`.
- `project` is the project's slug, found from the issue when the change names one. `issue` is the issue's ID when the change named one issue. `target` says what the change was about: `issue 7`, `12 issues`, `rule 3`, `pattern 1a2b3c4d`, `token ci`.
- `outcome` is `done`, `error` (the tool or the writer refused it, or it failed) or `denied` (the token's level does not reach it).
- `summary` says the change in a few words, from the arguments as sent, so a call the tool refused for its arguments is described all the same: `resolved, fixed in release 2.0.0`, `ignored: issues 4, 9, 12`, `logs 90 days, errors the default`, `added a shadow rule for level debug and below, service web`, `unlocked destructive changes`. A refused or failed change adds a note after `;`: a person's or the CLI's the reason, an agent's a fixed word (see [What a row never holds](#what-a-row-never-holds)), and a denied one what the call needs and what the token holds: `needs triage, holds read`.
- `repeats` counts the same row again, by the same actor, with the same client, action, project, issue, target, outcome and summary, each less than a minute after the last, and only while that row is the newest of the whole table. An agent that retries a refused call in a loop leaves one row with a count, and `at` is the time of the last. A change by anyone else in between starts a new row, so a count never hides what came between the first try and the last.

### What leaves a row

- **Over MCP**, every call of a tool that needs more than `read`, as it ended, and every call the token's level refused. A call of a read tool leaves none: an agent reads far more than it changes, and the token's `used_at` says when it last called. A request without a valid token is a `401` and a tool the table has no row for is `tool not found`; neither leaves a row, as neither has an actor or an action to name.
- **In the web UI**, a person's change of issue status (resolve, resolve in a release, ignore, reopen, one issue or many) and of retention, and on the Tokens page a change of level, an unlock or lock of destructive changes and a revocation. The row says whether the change was done or refused, and why.
- **With `davy`**, `davy resolve`, `davy project retention` with a setting, `davy drop live`, `davy drop add` of a live rule, `davy drop remove`, and `davy token add`, `level`, `unlock`, `lock` and `revoke`. `davy drop shadow`, `davy drop add --shadow` and `davy drop list` leave none: they stage or count and drop nothing. A remove does leave one: a removed live rule stops dropping, and a removed shadow rule is a person undoing what an agent staged.
- Logins, password changes, ingest, check-ins and alert deliveries leave none. They are not changes an agent makes, and each has its own record.

### What a row never holds

A summary is built from a few named arguments of each change, never from the arguments as a whole, so a field nobody listed never reaches the ledger. It never holds a token's secret, an event or log payload, or the text of `submit_feedback`. Texts an agent or a person sent are cut: a release, a service or a setting to 40 characters, a log message a rule matches to 60, a pattern to 20, an actor, client, project or target to 100, and the summary with its reason to 200. Control characters and marks that reorder text become spaces. An agent's refused or failed call keeps a fixed note, never the error's text: `tool error` when the tool refused it, `refused` when the writer did, `invalid arguments` when the arguments do not parse, and `protocol error CODE` otherwise. An error text can quote what the agent sent, a file path or a log line, and the agent got the text in its answer already. A person's or the CLI's refused change keeps the reason, cut with the summary: Davy wrote that text, from arguments a signed-in person or the host's operator gave.

### How a row is written

- A change from the web UI or the CLI and its row are written in one transaction, so a row never names a change that did not happen, and a change never lacks its row. The web UI sends both to the writer as one job, `Recorded`, whose row takes the change's outcome. The CLI writes both on its own connection, in one `BEGIN IMMEDIATE` transaction, so it waits for a server that writes.
- An agent's change that deletes data, an unlocked `drop_rule_live` or a `set_retention` that shortens, goes to the writer as one `Recorded` job too, with the token's row marked `destructive`. The writer reads the token's unlock again in that transaction, so a lock or revocation that came first refuses it, and the row says so. A crash never leaves such a change without its row.
- Any other change over MCP goes through the writer inside the tool, so its row is a second job, queued when the tool returns. The answer waits until the row is stored, so an agent that reads the ledger next sees it, and a full queue makes the call wait for room, so a busy agent slows down rather than rows piling up in memory. A crash between the change and its row loses the row; the change is in the store.
- Migration 27 adds `ledger(id, at, actor_kind, actor, client, action, project, issue, target, outcome, summary, repeats, destructive)`, with checks on `actor_kind`, `outcome` and `destructive`, and an index on `(actor, id)` for the filter.

### Retention

The sweep deletes rows older than 180 days. It also caps each agent actor, the admin token and each token name, by kind of row: the newest 5,000 done changes, the newest 500 refused or denied calls, and the newest 2,000 changes that deleted data. Each kind only pushes out older rows of its own kind and actor, so:

- an agent in a loop of refused calls keeps 500 rows and never pushes out its own changes, nor anyone else's rows;
- a flood of harmless changes never pushes out the record of a change that deleted data;
- a person's and the CLI's rows have no count cap: a person signs in to make each change, and the CLI runs on the host.

A row is about 200 bytes, so an agent actor holds about 1.5 MB at most, and 50 tokens with the admin token about 76 MB (an estimate, not measured). A token's rows stay after it is revoked, under its name, until they age out. It does not follow a project's retention: a row about a project outlives its logs, as the ledger is about the change, not the data.

### Reading it

- The MCP tool `ledger` takes `actor?`, `before?` and `limit?` (1 to 200, default 50) and returns `{rows, next_before}`, newest first. `next_before` is the ID to pass as `before` for the page before, null on the last page.
- It needs `read`. The ledger holds no secret and no payload, `sql` reads the table already, and every token reaches every project (see "Tenant scope in agent tools"), so a reading agent learns nothing it could not read. And an agent that reads it before acting sees that a person reopened what it resolved, or which rule another agent staged.
- `client`, `project`, `target` and `summary` hold text an agent sent: a client name, a slug, a release, a pattern, a message. The tool's description says those fields are data, not instructions, as every tool that returns stored text does.
- The web UI's Ledger page reads the same rows, see [Views](#views).
