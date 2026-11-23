# Changelog

What changes for someone who runs Davy, per release. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and versions follow [Semantic Versioning](https://semver.org/). How to add a line is in `CONTRIBUTING.md`.

## Unreleased

### Fixed

- A bare word in a log search now finds attribute values at any depth. Searching `r-3fa9c2d1` found nothing when the request ID sat in `attributes.req.id`, and only `attributes.req.id:r-3fa9c2d1` found it. Logs written before this release keep the old behaviour: a bare word matches their message only.
- An issue's latest event is now its newest one. After 50 events Davy kept the payload of only every 100th, so the UI, `davy event` and `issue_context` could show an event hours older than the issue's last seen time.

## 0.1.0 (2026-11-23)

The first release. One binary takes errors from the Sentry SDKs and logs from Logtail shippers and OpenTelemetry, checks uptime, sends alerts, serves a web UI for one team and gives coding agents MCP tools. Install it with [docs/install.md](docs/install.md) or the Docker quick start in the [README](README.md).

### Added

- **Errors.** Ingest from the Sentry SDKs through the envelope and `/store/` routes. Events group into issues, and a fixed issue reopens only when the error comes back in the fix release or a newer one.
- **Source maps.** `sentry-cli` uploads maps as it would to Sentry, and minified frames turn back into source by debug ID.
- **Logs.** Ingest from Logtail shippers (JSON and msgpack), OTLP/HTTP and Sentry's `log` item. Full-text search per UTC day, kept 30 days by default (`--log-retention-days`).
- **Uptime.** HTTP monitors with status and keyword checks, and heartbeats for cron jobs with a failure ping.
- **Alerts.** New issues, regressions and monitor incidents go to signed webhooks, Slack and e-mail over SMTP, with retries. Never one notice per event.
- **Agents.** An MCP server at `/mcp` with `schema`, `sql`, `search_logs`, `issue_context`, `mark_fixed` and `submit_feedback`, behind the admin token. `davy mcp-config` prints the config for Claude Code, Codex and Cursor. `davy feedback` lists what agents reported.
- **Web UI** at `/ui/`, for the people `davy user add` invites:
  - Issues: triage from the keyboard, a 24-hour strip per issue, resolve, ignore and reopen one issue or many.
  - An issue's events with stack traces, breadcrumbs, context and the logs around them.
  - Logs: search, filter by range, project and level, zoom on the histogram, live tail.
  - Uptime, Alerts with every delivery and its last error, Projects with each DSN and the code per SDK, Setup with the install steps Davy has seen, Account.
  - Light and dark, following the system.
- **Install.** `davy setup` prepares the data directory, the admin token and a launchd job or systemd unit in one command and can run again safely. `davy doctor` checks a running install and prints the fix for each problem, also as `--json`. `docs/install.md` is written for a coding agent to follow, with one page per SDK in `docs/connect/` and an index in `llms.txt`.
- **CLI.** `project`, `user`, `issues`, `event`, `context`, `resolve`, `logs`, `sql`, `monitor`, `channel` and `feedback`, each with `--json` where it prints data.
- **Downloads.** Static Linux binaries for x86_64 and aarch64, macOS binaries for Apple silicon and Intel, and the multi-arch image `ghcr.io/dominikzabcik/davy` on distroless as a non-root user. Each binary has a CycloneDX SBOM and build provenance, and the image is signed. [RELEASING.md](RELEASING.md) shows how to verify them.

### Performance

Measured on the release build:

- The issue list answers in under 30 ms at the 95th percentile with 100,000 issues.
- A page of logs answers in under 35 ms and a histogram of a whole day in under 50 ms at the 95th percentile with 10 million logs.
- When the ingest queue is full Davy answers 429 and the SDKs back off, and UI writes leave half of each write batch to ingest.

### Security

- An internal penetration test of the server found 1 high, 2 medium, 7 low and 6 informational findings. Two security reads of the fixes by other models followed. Every high is fixed. [The report](docs/security/pentest-0.1.0.md) lists each finding and what became of it.
- An internal penetration test of the web UI found 1 medium, 3 low and 7 informational findings, with two reads of the fixes after it. [The report](docs/security/pentest-ui-0.1.0.md) lists each finding and what became of it.
- Davy creates its files readable by its own user only, keeps the admin token and SMTP URL in files (`DAVY_TOKEN_FILE`, `DAVY_SMTP_URL_FILE`) and refuses a plain `http` MCP URL unless the host is a loopback address.
