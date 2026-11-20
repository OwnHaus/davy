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
- `davy setup` prepares a machine in one command: a private data directory, a random admin token in `DATA/token`, its settings in `DATA/setup.json` and a launchd job when asked. It prints the next commands as JSON and can run again safely.
- `davy systemd-unit --data DIR --user USER` prints a hardened systemd unit for `sudo tee`. It holds nothing from the data directory, so the service user cannot change what root installs.
- `davy serve --data DIR` takes the address, the public URL and the sender from `DIR/setup.json`, and the admin token and SMTP URL from `DIR/token` and `DIR/smtp-url`, when they are not given.
- The systemd unit waits until the server listens (`Type=notify`), so `systemctl restart davy` returns only when Davy answers.
- `davy doctor` checks a running install: permissions of the data directory and token, TLS, that the server answers and that its MCP endpoint takes the token, the SMTP server and free disk space. Every check that is not ok prints the command that fixes it, and `--json` gives the same for an agent. Control characters in what an SMTP server answers are printed as `�`, so the reply cannot write to the terminal.
- `davy mcp-config --client claude-code|codex|cursor` prints the MCP config for that client. It reads the token from `DAVY_TOKEN`, so the file can be committed, and refuses a plain `http` URL that leaves the machine.
- The MCP tool `submit_feedback` lets an agent report a tool that was missing, wrong or needed a workaround. `davy feedback` lists the reports, and a channel added with `--feedback` receives each one. Davy sends them nowhere else, and at most 5 wait unsent per channel, so they cannot crowd out alerts.
- `davy project create` takes the public URL from `davy setup` and prints the existing DSN when the project is already there, so it can run again safely.
- An install guide for coding agents in `docs/install.md`, one page per SDK in `docs/connect/` and an index in `llms.txt`.
- The CLI commands `project`, `issues`, `event`, `logs`, `sql`, `context`, `resolve`, `monitor` and `channel`.
