# Connect a Rust app

Use the official `sentry` SDK. Start with `davy connect my-app --data DATA
--sdk rust` on the Davy host. Keep `SENTRY_DSN` in an ignored `.env` exported
by your launcher, or a systemd drop-in. Rust does not load `.env` itself.

## 1. Install

<!-- e2e: skip, requires a Rust application -->
```sh
cargo add sentry@0.49
```

## 2. Start with the app

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dsn = std::env::var("SENTRY_DSN")?.parse()?;
    let _guard = sentry::init(sentry::ClientOptions::new().dsn(Some(dsn)));
    sentry::capture_message("Davy test error", sentry::Level::Error);
    Ok(())
}
```

Keep the guard alive until shutdown so queued events flush. Initialize it
before starting an async runtime.

The [current ClientOptions docs](https://docs.rs/sentry/0.49.6/sentry/struct.ClientOptions.html)
mark the type non-exhaustive. Use the builder above or mutate
`ClientOptions::default()`; a struct literal is not supported in 0.49.
Older 0.4x examples with `..Default::default()` apply only to versions whose
options type permits struct construction.

For tracing, install `sentry-tracing` and `tracing-subscriber`:

<!-- e2e: skip, requires a Rust application -->
```sh
cargo add sentry-tracing@0.49 tracing tracing-subscriber
```

After Sentry init, register an error layer:

```rust
use tracing_subscriber::prelude::*;
use tracing_subscriber::filter::LevelFilter;

tracing_subscriber::registry()
    .with(sentry_tracing::ErrorLayer::new().with_filter(LevelFilter::ERROR))
    .init();
tracing::error!("Davy test error");
```

The [0.49.6 tracing guide](https://docs.rs/sentry-tracing/0.49.6/sentry_tracing/)
replaces the deprecated combined `sentry_tracing::layer()` with separate
layers. Use `ErrorLayer` for issues; `LogLayer` (with its `logs` feature) is
for structured logs. Davy drops transactions and spans, so no `SpanLayer`
or trace sampling is needed here.

## 3. Check

Run the returned `test_event` command first to check ingest. Run the app's
capture above, then poll MCP `project_status` for `my-app` and check that
`event_count` increases. `issue_context` reads the resulting issue's stack.
This guide was checked against SDK docs, not a Rust-to-Davy runtime test.
