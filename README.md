# Davy

One self-hosted binary that replaces Sentry and Better Stack. Point your existing Sentry SDK or Logtail shipper at it by changing the DSN or source token. No code changes.

Status: errors, logs, source maps, uptime checks, heartbeats, alerts and MCP work. The UI comes later. See `docs/design.md`.

## Install with a coding agent

Give your agent this prompt in the app's repository:

```text
Install Davy on this machine and connect this app to it.
Follow docs/install.md in https://github.com/dominikzabcik/davy from top to bottom.
```

The agent asks you for the public URL, and optionally an SMTP server, and does the rest: binary, service, TLS, the SDK in your app and MCP for itself. `davy doctor` tells it what is still wrong and how to fix it. The pages it follows are listed in `llms.txt`.

## Try it locally

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

## Coding agents

`davy setup` writes an admin token, and `/mcp` turns on when the server has one. `davy mcp-config` prints the config for Claude Code, Codex or Cursor:

```sh
./target/release/davy setup --data ./data --public-url http://127.0.0.1:8000
DAVY_TOKEN_FILE=./data/token ./target/release/davy serve --data ./data --listen 127.0.0.1:8000 --public-url http://127.0.0.1:8000
export DAVY_TOKEN="$(cat ./data/token)"
./target/release/davy mcp-config --client claude-code --data ./data
```

The agent gets six tools: `schema`, `sql` (one read-only SQLite query), `search_logs`, `issue_context` (stack with source lines, breadcrumbs, releases and the logs around the error), `mark_fixed` and `submit_feedback`, which reports a problem with Davy's own tools to whoever runs it (`davy feedback`). A fixed issue reopens when the error comes back in the fix release or a newer one, not from older releases still running.

The same works from a shell:

```sh
./target/release/davy sql --data ./data "SELECT id, title, event_count FROM issues WHERE status = 'unresolved'"
./target/release/davy context 12 --data ./data
./target/release/davy resolve 12 --data ./data --release 1.4.0
```

## Source maps

With `DAVY_TOKEN` set, `sentry-cli` uploads source maps to Davy as it would to Sentry. Run `inject` before you deploy the bundle, so the SDK reports the debug ID that ties an error to its map:

```sh
export SENTRY_URL=http://127.0.0.1:8000 SENTRY_AUTH_TOKEN=$DAVY_TOKEN SENTRY_ORG=any SENTRY_PROJECT=my-app
npx @sentry/cli sourcemaps inject dist
npx @sentry/cli sourcemaps upload --release my-app@1.0.0 dist
```

Errors that arrive after the upload show the original function, file, line and source. Davy matches by debug ID only, so the org, project and release names do not matter.

## Uptime and alerts

```sh
./target/release/davy monitor add my-app homepage --url https://example.com --keyword Welcome --data ./data
./target/release/davy monitor add my-app nightly-backup --heartbeat --every 24h --data ./data
./target/release/davy channel add slack https://hooks.slack.com/services/... --all --data ./data
```

A heartbeat prints the path the job pings when it finishes. Channels get a notice when a monitor goes down or comes back and when an issue is new or regressed, never on every event. Webhook notices are signed with a secret printed once.

## Tests

```sh
cargo test
bash e2e/run.sh    # needs Node and pnpm; runs real @sentry/node 7 and 10, @logtail/pino, the OpenTelemetry exporters, the MCP client, sentry-cli and the alert receivers against the binary
bash e2e/install.sh # needs Docker; follows docs/install.md and the connect pages in Debian with systemd
```

## Contributing, releases and security

How changes land is in `CONTRIBUTING.md`, how versions ship in `RELEASING.md`, what changed in `CHANGELOG.md`. Report a vulnerability as `SECURITY.md` says, never in a public issue.

## License

AGPL-3.0. See `LICENSE`.
