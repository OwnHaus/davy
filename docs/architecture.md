# Architecture

A map of the code for whoever changes it next. It says where things live and which rules hold across modules. What each milestone does and why is in `docs/design.md`.

## The shape

One binary, `davy`, with subcommands. `davy serve` takes HTTP ingest and writes to disk. Every other subcommand reads or edits the same data directory from its own process.

```
SDK / shipper ──HTTP──> http.rs ──parse──> envelope.rs, event.rs, logs/{logtail,otlp,sentry}.rs
                           │                        (pure: bytes in, typed records out)
                           ├── errors ──> store::Writer (one thread) ──> data/davy.sqlite (SQLite, WAL)
                           └── logs ────> logs::store::LogWriter (one thread) ──> data/logs/YYYY-MM-DD/ (Tantivy)
CLI (main.rs) ──reads──> store.rs (SQLite), logs::store::search (Tantivy)
```

## Modules

- `src/main.rs` parses the command line and calls into the library. No logic of its own beyond formatting output.
- `src/http.rs` is the HTTP boundary. Routes, auth by project key, size caps, decompression, status codes. It turns bytes into typed values by calling the parsers, then hands them to a writer queue. It decides nothing about grouping or storage.
- `src/envelope.rs` parses the Sentry envelope format and decides what happens to each item type (`disposition`).
- `src/event.rs` parses a Sentry event JSON into `ErrorEvent`, leniently. This is the one place that knows the Sentry event schema.
- `src/time.rs` reads the clock, parses timestamp strings and maps instants to UTC days.
- `src/grouping.rs` is a pure function from `ErrorEvent` to a fingerprint and title. Versioned, because changing it regroups issues.
- `src/store.rs` owns the SQLite schema, the single error writer thread and the read queries the CLI uses.
- `src/logs/mod.rs` defines `LogRecord`, `LogLevel` and the timestamp and level rules shared by every log source.
- `src/logs/{logtail,otlp,sentry}.rs` are the log adapters. Each is a pure function from a request body to `Vec<LogRecord>`.
- `src/logs/store.rs` owns the Tantivy schema, the log writer thread, retention and search.
- `src/tests.rs` and `src/logs/tests.rs` hold the unit and contract tests. `e2e/` runs real SDKs against the built binary. `bench/` and `examples/load_logs.rs` are benchmarks, not tests.

## Invariants

- **Parsers are pure.** Everything that turns wire bytes into domain types takes bytes and returns a value or an error. No I/O, no clock reads except a passed-in `observed_at`, no global state. Each is tested on fixtures captured from real clients.
- **Validation happens once, at the boundary.** `http.rs` and the parsers check input. Code behind them trusts the types and does not re-check.
- **One writer thread per store.** Only `store::Writer` writes SQLite and only `LogWriter` writes Tantivy. HTTP handlers queue work and never touch the database or the index directly. A full queue is a `429`, never an unbounded buffer.
- **Readers live in other processes.** The CLI opens the data directory while the server writes. SQLite WAL and Tantivy's segment files make that safe. Search retries once on a missing segment file.
- **Lenient on fields, strict on framing.** A malformed envelope or body is an error status. A wrong field inside a valid record falls back to a default, and one bad record never fails a batch.
- **Time is unix nanoseconds for logs and unix milliseconds for error events** (`event::Timestamp`), both `i64`. `src/time.rs` owns clock reads, string parsing and UTC days. Every log source goes through `logs::timestamp`.
- **The open core boundary.** This crate is the self-hosted product and must stay usable as a library by the closed `davy-cloud` crate. Nothing here may depend on cloud code, and cloud features must not need a fork.

## Where to add things

- A new log source is a new adapter in `src/logs/`, a route in `http.rs`, a captured fixture in `e2e/fixtures/` and an e2e sender.
- A new Sentry item type is a `disposition` change in `envelope.rs` and a handler arm in `http.rs`.
- A new CLI command is a `Command` variant in `main.rs` that calls a library function.
- A new column or index is a schema change in `store.rs` or `logs/store.rs`. Existing data directories must keep opening. Say in `docs/design.md` what happens to old data.
