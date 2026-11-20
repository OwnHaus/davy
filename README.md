# Davy

One self-hosted binary that replaces Sentry and Better Stack. Point your existing Sentry SDK or Logtail shipper at it by changing the DSN or source token. No code changes.

Status: error ingest works (Sentry envelope and `/store/`, grouping into issues, CLI). Logs, MCP for coding agents, UI, uptime and source maps come next. See `docs/design.md`.

## Try it

```sh
cargo build --release
./target/release/davy project create my-app --data ./data --public-url http://127.0.0.1:8000
./target/release/davy serve --data ./data --listen 127.0.0.1:8000 --public-url http://127.0.0.1:8000
./target/release/davy issues --data ./data
```

`project create` prints a DSN. Put it into `Sentry.init({ dsn })`.

## Tests

```sh
cargo test
bash e2e/run.sh    # needs Node and pnpm; runs real @sentry/node 7 and 10 against the binary
```

## License

AGPL-3.0. See `LICENSE`.
