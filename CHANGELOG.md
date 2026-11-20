# Changelog

What changes for someone who runs Davy, per release. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and versions follow [Semantic Versioning](https://semver.org/). How to add a line is in `CONTRIBUTING.md`.

## Unreleased

### Added

- Error ingest from the Sentry SDKs through the envelope and `/store/` routes, grouped into issues that reopen on a regression in the fix release or a newer one.
- Log ingest from Logtail shippers (JSON and msgpack), OTLP/HTTP and the Sentry `log` item, with full-text search kept per UTC day and a retention in days.
- Source map upload from `sentry-cli` and symbolication of minified stack frames by debug ID.
- An MCP server for coding agents with the tools `schema`, `sql`, `search_logs`, `issue_context` and `mark_fixed`, behind an admin token from `DAVY_TOKEN`.
- HTTP uptime monitors with status and keyword checks, and heartbeat monitors for cron jobs with a failure ping.
- Alerts for new issues, regressions and monitor incidents to signed webhooks, Slack and e-mail over SMTP, with retries.
- The admin token and the SMTP URL can come from files named by `DAVY_TOKEN_FILE` and `DAVY_SMTP_URL_FILE`, the way systemd credentials and container secrets hand them over.
- `davy setup` prepares a machine in one command: a private data directory, a random admin token in `DATA/token` and a hardened systemd unit or launchd job. It prints the next commands as JSON and can run again safely.
- `davy doctor` checks a running install: permissions of the data directory and token, TLS, that the server answers and runs with the token, the SMTP server and free disk space. Every check that is not ok prints the command that fixes it, and `--json` gives the same for an agent.
- `davy mcp-config --client claude-code|codex|cursor` prints the MCP config for that client. It reads the token from `DAVY_TOKEN`, so the file can be committed.
- `davy project create` takes the public URL from `davy setup` and prints the existing DSN when the project is already there, so it can run again safely.
- An install guide for coding agents in `docs/install.md`, one page per SDK in `docs/connect/` and an index in `llms.txt`.
- The CLI commands `project`, `issues`, `event`, `logs`, `sql`, `context`, `resolve`, `monitor` and `channel`.
