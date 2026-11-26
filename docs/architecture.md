# Architecture

A map of the code for whoever changes it next. It says where things live and which rules hold across modules. What each milestone does and why is in `docs/design.md`.

## The shape

One binary, `davy`, with subcommands. `davy serve` takes HTTP ingest, writes to disk and, with a token, serves MCP to coding agents. Every other subcommand reads or edits the same data directory from its own process.

```
SDK / shipper ──HTTP──> http.rs ──parse──> envelope.rs, event.rs, logs/{logtail,otlp,sentry,vercel}.rs
                           │                        (pure: bytes in, typed records out)
                           ├── errors ──> sourcemaps::symbolicate ──> store::Writer (one thread) ──> data/davy.sqlite (SQLite, WAL)
                           └── logs ────> logs::store::LogWriter (one thread) ──> data/logs/YYYY-MM-DD/ (Tantivy)
cron job ──HTTP /api/heartbeat──> http.rs ──> store::Writer ──> monitor::store
monitor::check::run (task) ──reads due monitors, HTTP GET──> checked sites ──result──> store::Writer ──> monitor::store
                                    store::Writer ──new issue, regression, incident──> alert::store::notify (outbox rows, same transaction)
                                    alert::deliver::run (task) ──reads due rows, renders digests, HTTP POST or SMTP──> webhook, Slack, e-mail ──result──> store::Writer
sentry-cli ──HTTP──> upload.rs ──> sourcemaps::Store ──> data/sourcemaps/<debug-id>.{map,js}.zst
agent ──HTTP /mcp──> mcp.rs ──reads──> query.rs, context.rs, logs::store::search
                       └── mark_fixed ──> store::Writer
CLI (main.rs) ──reads──> store.rs, query.rs, context.rs, logs::store::search
```

## Modules

- `src/main.rs` parses the command line and calls into the library. No logic of its own beyond formatting output.
- `src/http.rs` is the HTTP boundary. Routes, auth by project key, size caps, decompression, status codes; the log routes live in `src/http/logs.rs`. It turns bytes into typed values by calling the parsers, then hands them to a writer queue. It decides nothing about grouping or storage.
- `src/envelope.rs` parses the Sentry envelope format and decides what happens to each item type (`disposition`).
- `src/event.rs` parses a Sentry event JSON into `ErrorEvent`, leniently. This is the one place that knows the Sentry event schema.
- `src/time.rs` reads the clock, parses timestamp strings and `--since` style instants (`30m`, RFC 3339), formats RFC 3339, and maps instants to UTC days.
- `src/grouping.rs` is a pure function from `ErrorEvent` to a fingerprint and title. Versioned, because changing it regroups issues.
- `src/store.rs` owns the SQLite schema and its migrations (`store/migrations.rs`, counted in `PRAGMA user_version`, run by `store/migrate.rs` only while no other process has the database open), the issue status and regression rule, the single error writer thread (`store/writer.rs`) and the read queries the CLI uses.
- `src/query.rs` runs one read-only SQL statement for an agent, behind an authorizer, with row, text and time limits.
- `src/context.rs` gathers one issue with its compacted latest event, recent events, releases and nearby logs.
- `src/accounts/` holds the people of a self-hosted Davy: e-mail, password and secret rules and the `Accounts` trait the UI asks for the person (`mod.rs`), the user, invite and session rows and the changes the writer applies to them (`store.rs`), the per-e-mail login backoff (`backoff.rs`) and `davy user` (`cli.rs`).
- `src/ui/` is the web UI boundary: the routes, the `Host` and `Origin` guard and the page headers (`mod.rs`), login, logout, welcome and a person's own password and sessions (`account.rs`), the cookie and the SQLite `Accounts` (`session.rs`), and the UI build that `build.rs` embeds from `ui/dist/` (`assets.rs`).
- `src/token.rs` holds the admin token and the middleware that requires it.
- `src/upload.rs` is the `sentry-cli` boundary: chunk upload and artifact bundle assembly behind the admin token.
- `src/sourcemaps/` stores uploaded builds per debug ID (`mod.rs`), unpacks artifact bundles (`bundle.rs`) and rewrites minified frames in an event before it is parsed (`symbolicate.rs`, pure apart from the build lookup).
- `src/mcp.rs` is the MCP boundary: allowed hosts behind the admin token, and the tools as thin calls into `query`, `context`, `logs::store::search` and the writer queue.
- `src/monitor/` holds uptime monitors: what a monitor watches, its limits and the state machine a check result moves (`mod.rs`), the HTTP check and its scheduler task (`check.rs`), the monitor rows (`store.rs`), and cron check-ins from the Sentry SDKs: the item parser (`checkin.rs`), the schedule with its crontab and time zone (`schedule.rs`) and the writer's rules for a check-in (`checkin_store.rs`). The tables are created by `store.rs` migrations like every other table.
- `src/alert/` holds alert channels: their kinds and signing secret (`mod.rs`), what a notice says and the body each channel kind gets (`notice.rs`, pure), the one message for issue notices that waited together (`digest.rs`, pure), the channel and outbox rows (`store.rs`), the SMTP sender of e-mail rows (`mail.rs`), and the notifier task that sends due rows and stores each result through the writer queue (`deliver.rs`). `store::insert` and `monitor::store` call `alert::store::notify` inside the writer's transaction.
- `src/net.rs` holds what checks and deliveries share when they reach a URL a user typed: the client, the `Reach` it enforces (any address, or public ones only for the cloud), the User-Agent and the error text that never holds the URL.
- `src/tasks.rs` holds `poll`, the loop behind each background task in `davy serve` that reads due work from SQLite: one task per key, a limit on how many run, and the result stored through the writer before the key is free.
- `src/logs/mod.rs` defines `LogRecord`, `LogLevel` and the timestamp and level rules shared by every log source.
- `src/logs/{logtail,otlp,sentry,vercel}.rs` are the log adapters. Each is a pure function from a request body to `Vec<LogRecord>`.
- `src/http/vercel.rs` is the Vercel Log Drain route and its `x-vercel-signature` check, and `src/vercel.rs` keeps a project's drain secret and `davy project vercel`.
- `src/logs/store.rs` owns the Tantivy schema, the log writer thread and search. `src/logs/retention.rs` deletes from it the logs each project keeps no longer.
- `src/logs/patterns.rs` masks the values in a log message to its pattern, a pure function. `src/logs/tally.rs` counts records and bytes per pattern in the writer thread and keeps them in a file per project in each day's directory (`src/logs/tally_file.rs`), and `src/logs/pattern_report.rs` reads them for `log_patterns`.
- `src/retention.rs` holds how long each project keeps its logs and errors: the server defaults, a project's own settings, their limits and the change the writer applies. `src/retention/errors.rs` deletes expired events and user feedback on the writer thread and keeps issue counts, first seen and tags true.
- `src/self_report.rs` is the one way to report an operational problem: stderr, and while `davy serve` runs, a record in Davy's own project (`self_report/own.rs`, marked in `projects.own`) through a bounded, rate-limited sink (`self_report/limit.rs`) and the forwarder thread (`self_report/forward.rs`). `self_report/crash.rs` saves a panic in DATA/crashes from the panic hook, and the forwarder records it as an issue. `self_report/status.rs` sums up the last 24 hours for `project_status` and the doctor check `davy` (`install/doctor_davy.rs`). See "Self-monitoring" in `design.md`.
- `src/usage.rs` counts the records and bytes ingest accepts per day, project and signal, and flushes them to `usage_daily` through the writer every 5 s. `src/quota.rs` keeps each project's daily quotas and today's counts in memory, which ingest asks before it reads a body (`src/http/limit.rs`), and flushes what it refused to `client_reports` and `refused_requests` with the usage counts. `refused_requests` also counts the `429`s of a full body budget or queue, as busy. `src/usage/disk.rs` measures the data directory, `src/usage/forecast.rs` is the pure forecast of when the disk fills, and `src/usage/report.rs` builds the answer of the `usage` MCP tool and `davy usage`.
- `src/tests.rs` and `src/logs/tests.rs` hold the unit and contract tests. `e2e/` runs real SDKs against the built binary. `bench/` and `examples/load_logs.rs` are benchmarks, not tests.

## Trust boundaries

Six kinds of caller reach the server. Each route belongs to exactly one.

| Caller | Credential | Routes | May do |
|---|---|---|---|
| SDK or log shipper | Project ingest key, public | `/api/{project}/envelope/`, `/api/{project}/store/`, `/`, `/v1/logs` | Write events and logs into the one project the key names, up to the project's daily quota, past which the route answers `429` before it reads the body, which it drains unread |
| Vercel Log Drain | Project ingest key, public, in a custom `Authorization` header; plus `x-vercel-signature`, the HMAC of the body as sent under the drain secret, when the project has one. The secret guards this route only: the same key still writes through the SDK routes above | `/vercel/logs` | Write logs into the one project the key names, up to the project's daily quota, checked after the signature; past it the route answers `200` and drops the records, as Vercel turns off a drain that keeps failing |
| Heartbeat sender, such as a cron job | Heartbeat token, secret, in the path | `/api/heartbeat/{token}`, `/api/heartbeat/{token}/fail` | Record a ping or a failure for the one monitor the token names |
| Coding agent | Admin token, secret | `/mcp`, `/api/projects/{project}/scrub` | Read everything, mark issues fixed, set how long a project keeps its logs and errors, tighten project scrubbing |
| `sentry-cli` | Admin token, secret | `/api/0/organizations/{org}/chunk-upload/`, `.../artifactbundle/assemble/` | Write source map builds |
| Person in a browser | Session cookie `__Host-davy_session`, secret, from a login or a welcome link | `/ui/api/*` but login, logout and welcome | Read everything, change the status of issues, set how long a project keeps its logs and errors, change their own password, end their own sessions |

The pages under `/ui/` and the login, logout and welcome routes need no credential. The admin token opens nothing under `/ui/`, and the session cookie opens nothing outside it. A UI write also needs `Content-Type: application/json` and the exact `Origin` of the public URL.

The CLI is a seventh caller. `davy user add` prints the welcome link a person sets their password with. It runs on the host with file access to the data directory and needs no credential. The cloud service adds tenants above projects. A cloud feature must not let one tenant's key or token reach another tenant's data, so every query the cloud runs carries the tenant in its type, not in a string filter.

## Invariants

- **Parsers are pure.** Everything that turns wire bytes into domain types takes bytes and returns a value or an error. No I/O, no clock reads except a passed-in `observed_at`, no global state. Each is tested on fixtures captured from real clients.
- **Validation happens once, at the boundary.** `http.rs` and the parsers check input. Code behind them trusts the types and does not re-check.
- **One writer thread per store.** Inside the server, only `store::Writer` writes SQLite and only `LogWriter` writes Tantivy, retention included. HTTP handlers and MCP tools queue work and never write directly. A full queue is a `429` or a tool error, never an unbounded buffer. The SQLite writer has a second queue for jobs from people in the UI and takes from it first, so a login commits while ingest fills the first queue. CLI commands that write (`project create`, `project retention`, `project vercel`, `resolve`) use their own connection, and SQLite's lock orders them with the writer. That lock does not cover Tantivy, so a retention change from the CLI is not ordered with a log deletion already under way, see "Retention per project" in `design.md`. Before the writers start, `davy serve` finds or makes Davy's own project on a connection of its own (`self_report::own`).
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
