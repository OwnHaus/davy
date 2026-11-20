# Davy

One self-hosted binary that replaces Sentry and Better Stack. Point your existing Sentry SDK or Logtail shipper at it by changing the DSN or source token. No code changes.

Status: error ingest works (Sentry envelope and `/store/`, grouping into issues, CLI). Log ingest works from Logtail shippers, OpenTelemetry and Sentry's logger, with full-text search from the CLI. MCP for coding agents, UI, uptime and source maps come next. See `docs/design.md`.

## Try it

```sh
cargo build --release
./target/release/davy project create my-app --data ./data --public-url http://127.0.0.1:8000
./target/release/davy serve --data ./data --listen 127.0.0.1:8000 --public-url http://127.0.0.1:8000
./target/release/davy issues --data ./data
```

`project create` prints a DSN. Put it into `Sentry.init({ dsn })`.

## Logs

The DSN's public key is also the log token. Three ways in:

- **Logtail shippers** (`@logtail/pino`, `@logtail/node`, Vector, Fluent Bit). Set the endpoint to `http://127.0.0.1:8000` and the source token to the public key.
- **OpenTelemetry.** `OTEL_EXPORTER_OTLP_LOGS_ENDPOINT=http://127.0.0.1:8000/v1/logs` and `OTEL_EXPORTER_OTLP_HEADERS=Authorization=Bearer <public_key>`. Protobuf and JSON both work.
- **Sentry's logger.** `Sentry.init({ dsn, enableLogs: true })`, then `Sentry.logger.info(...)`.

Or by hand:

```sh
curl -X POST http://127.0.0.1:8000/ -H 'Authorization: Bearer <public_key>' \
  -H 'Content-Type: application/json' -d '{"message":"payment timed out","level":"error","provider":"acme"}'
```

Search them:

```sh
./target/release/davy logs --data ./data --project my-app --since 1h --level warn 'timeout AND attributes.provider:acme'
```

`--since` and `--until` take `30m`, `2h`, `7d` or an RFC 3339 time (default the last 24 h). The query uses Tantivy syntax over the message and the attributes. `--json` prints one hit per line. Logs are kept 30 days by default (`davy serve --log-retention-days`).

## Tests

```sh
cargo test
bash e2e/run.sh    # needs Node and pnpm; runs real @sentry/node 7 and 10, @logtail/pino and the OpenTelemetry exporters against the binary
```

## License

AGPL-3.0. See `LICENSE`.
