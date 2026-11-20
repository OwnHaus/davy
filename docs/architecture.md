# Architecture

A map of the code for whoever changes it next. It says where things live and which rules hold across modules. What each milestone does and why is in `docs/design.md`.

## The shape

One binary, `davy`, with subcommands. `davy serve` takes HTTP ingest, writes to disk and, with a token, serves MCP to coding agents. Every other subcommand reads or edits the same data directory from its own process.

```
SDK / shipper ──HTTP──> http.rs ──parse──> envelope.rs, event.rs, logs/{logtail,otlp,sentry}.rs
                           │                        (pure: bytes in, typed records out)
                           ├── errors ──> sourcemaps::symbolicate ──> store::Writer (one thread) ──> data/davy.sqlite (SQLite, WAL)
                           └── logs ────> logs::store::LogWriter (one thread) ──> data/logs/YYYY-MM-DD/ (Tantivy)
sentry-cli ──HTTP──> upload.rs ──> sourcemaps::Store ──> data/sourcemaps/<debug-id>.{map,js}.zst
agent ──HTTP /mcp──> mcp.rs ──reads──> query.rs, context.rs, logs::store::search
                       └── mark_fixed ──> store::Writer
CLI (main.rs) ──reads──> store.rs, query.rs, context.rs, logs::store::search
```

## Modules

- `src/main.rs` parses the command line and calls into the library. No logic of its own beyond formatting output.
- `src/http.rs` is the HTTP boundary. Routes, auth by project key, size caps, decompression, status codes. It turns bytes into typed values by calling the parsers, then hands them to a writer queue. It decides nothing about grouping or storage.
- `src/envelope.rs` parses the Sentry envelope format and decides what happens to each item type (`disposition`).
- `src/event.rs` parses a Sentry event JSON into `ErrorEvent`, leniently. This is the one place that knows the Sentry event schema.
- `src/time.rs` reads the clock, parses timestamp strings and `--since` style instants (`30m`, RFC 3339), formats RFC 3339, and maps instants to UTC days.
- `src/grouping.rs` is a pure function from `ErrorEvent` to a fingerprint and title. Versioned, because changing it regroups issues.
- `src/store.rs` owns the SQLite schema and its migrations (`PRAGMA user_version`), the issue status and regression rule, the single error writer thread and the read queries the CLI uses.
- `src/query.rs` runs one read-only SQL statement for an agent, behind an authorizer, with row, text and time limits.
- `src/context.rs` gathers one issue with its compacted latest event, recent events, releases and nearby logs.
- `src/token.rs` holds the admin token and the middleware that requires it.
- `src/upload.rs` is the `sentry-cli` boundary: chunk upload and artifact bundle assembly behind the admin token.
- `src/sourcemaps/` stores uploaded builds per debug ID (`mod.rs`), unpacks artifact bundles (`bundle.rs`) and rewrites minified frames in an event before it is parsed (`symbolicate.rs`, pure apart from the build lookup).
- `src/mcp.rs` is the MCP boundary: allowed hosts behind the admin token, and the tools as thin calls into `query`, `context`, `logs::store::search` and the writer queue.
- `src/monitor/` holds uptime monitors: what a monitor watches, its limits and the state machine a check result moves (`mod.rs`), and the monitor rows (`store.rs`). The tables are created by `store.rs` migrations like every other table.
- `src/logs/mod.rs` defines `LogRecord`, `LogLevel` and the timestamp and level rules shared by every log source.
- `src/logs/{logtail,otlp,sentry}.rs` are the log adapters. Each is a pure function from a request body to `Vec<LogRecord>`.
- `src/logs/store.rs` owns the Tantivy schema, the log writer thread, retention and search.
- `src/tests.rs` and `src/logs/tests.rs` hold the unit and contract tests. `e2e/` runs real SDKs against the built binary. `bench/` and `examples/load_logs.rs` are benchmarks, not tests.

## Trust boundaries

Three kinds of caller reach the server. Each route belongs to exactly one.

| Caller | Credential | Routes | May do |
|---|---|---|---|
| SDK or log shipper | Project ingest key, public | `/api/{project}/envelope/`, `/api/{project}/store/`, `/`, `/v1/logs` | Write events and logs into the one project the key names |
| Coding agent | Admin token, secret | `/mcp` | Read everything, mark issues fixed |
| `sentry-cli` | Admin token, secret | `/api/0/organizations/{org}/chunk-upload/`, `.../artifactbundle/assemble/` | Write source map builds |

The CLI is a fourth caller. It runs on the host with file access to the data directory and needs no credential. The cloud service adds tenants above projects. A cloud feature must not let one tenant's key or token reach another tenant's data, so every query the cloud runs carries the tenant in its type, not in a string filter.

## Invariants

- **Parsers are pure.** Everything that turns wire bytes into domain types takes bytes and returns a value or an error. No I/O, no clock reads except a passed-in `observed_at`, no global state. Each is tested on fixtures captured from real clients.
- **Validation happens once, at the boundary.** `http.rs` and the parsers check input. Code behind them trusts the types and does not re-check.
- **One writer thread per store.** Inside the server, only `store::Writer` writes SQLite and only `LogWriter` writes Tantivy. HTTP handlers and MCP tools queue work and never write directly. A full queue is a `429` or a tool error, never an unbounded buffer. CLI commands that write (`project create`, `resolve`) use their own connection, and SQLite's lock orders them with the writer.
- **Source maps are files, not rows.** `upload.rs` writes chunks and builds under `data/chunks/` and `data/sourcemaps/` by writing a temporary file and renaming it. A build is named by its debug ID, a content hash, so a second upload of the same build writes the same bytes and nothing needs a lock.
- **Readers live in other processes.** The CLI opens the data directory while the server writes. SQLite WAL and Tantivy's segment files make that safe. Search retries once on a missing segment file.
- **Lenient on fields, strict on framing.** A malformed envelope or body is an error status. A wrong field inside a valid record falls back to a default, and one bad record never fails a batch.
- **Time is unix nanoseconds for logs and unix milliseconds for error events** (`event::Timestamp`), both `i64`. `src/time.rs` owns clock reads, string parsing and UTC days. Every log source goes through `logs::timestamp`.
- **The open core boundary.** This crate is the self-hosted product and must stay usable as a library by the closed `davy-cloud` crate. Nothing here may depend on cloud code, and cloud features must not need a fork.

## Where to add things

- A new log source is a new adapter in `src/logs/`, a route in `http.rs`, a captured fixture in `e2e/fixtures/` and an e2e sender.
- A new Sentry item type is a `disposition` change in `envelope.rs` and a handler arm in `http.rs`.
- A new CLI command is a `Command` variant in `main.rs` that calls a library function.
- A new MCP tool is a method in `mcp.rs` that calls a library function, a row in the tools table in `docs/design.md`, and a call in `e2e/mcp.mjs`.
- A new column or index is a schema change in `store.rs` or `logs/store.rs`. Existing data directories must keep opening. Say in `docs/design.md` what happens to old data.
