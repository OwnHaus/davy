# Connect a coding agent

Davy serves MCP at `/mcp` of the public URL. Claude Code, Codex and Cursor connect with the admin token and get thirteen tools: `schema`, `sql`, `search_logs`, `count_logs`, `log_patterns`, `release_health`, `issue_context`, `mark_fixed`, `submit_feedback`, `connect_project`, `project_status`, `set_retention` and `usage`.

The token gives full read access to errors and logs. It goes into a file only the person can read and reaches the client through the `DAVY_TOKEN` environment variable, never into a config file or a command line.

## 1. Give the person the token

On the Davy server, copy it into the person's home directory. It is read as the davy user, never as root, so a link in the data directory cannot point the copy at a file only root may read:

```sh
mkdir -p ~/.config/davy
(umask 077 && sudo runuser -u davy -- cat /var/lib/davy/token > ~/.config/davy/token)
```

From a laptop, fetch it over SSH: `(umask 077 && ssh SERVER sudo runuser -u davy -- cat /var/lib/davy/token > ~/.config/davy/token)`.

Then load it into the environment the client starts from. Use the profile of the person's shell, `~/.zshrc` on a Mac and usually `~/.bashrc` on Linux:

```sh
echo 'export DAVY_TOKEN="$(cat ~/.config/davy/token)"' >> ~/.bashrc
```

## 2. Add the server to the client

In the project the agent works on, print the config for the client:

```sh
davy mcp-config --client claude-code --url https://davy.example.com
```

Use `--client codex` or `--client cursor` for those clients. The output names the file and what to merge into it: `.mcp.json` for Claude Code, `.cursor/mcp.json` for Cursor, `~/.codex/config.toml` for Codex. The config refers to `DAVY_TOKEN`, so it can be committed. On a machine without the `davy` binary, write the config from the output of the same command on the server.

## 3. Restart the client

The client reads `DAVY_TOKEN` when it starts, so the person opens a new terminal and starts the client again. Claude Code asks once whether to trust the servers in `.mcp.json`.

To check, ask the agent: "List the unresolved issues in Davy." It calls `sql` and answers with the issues.

## What the agent can do

- `issue_context` returns what fixing an issue needs: the stack with source lines, breadcrumbs, the releases it occurs in, the logs around it and what users wrote about it through the SDK's feedback widget.
- `search_logs` searches logs over a time range. A bare word such as a request ID matches the message and every attribute value, and `attributes.<key>:<word>` matches one attribute.
- `count_logs` takes the filters of `search_logs` and returns how many logs match per time bucket. The agent sees how often something happens and since when before it reads examples.
- `log_patterns` lists what fills the logs of a window: each line shape with its values masked, an example, records, bytes, share of the bytes, levels and services, the most bytes first. Pass a row's `id` as `pattern` to `search_logs` or `count_logs` to read or count its lines.
- `sql` runs one read-only query, and `schema` describes the tables. The `client_reports` table says how many errors and logs each SDK dropped before sending, and why, so the agent can tell a quiet app from one whose `beforeSend` drops everything.
- `release_health` returns crash-free sessions and users per release and environment, newest release first, so the agent can tell whether a deploy made things worse. Browser and mobile SDKs send one session per user visit. Server SDKs count requests as sessions and name no users.
- Davy's own warnings, errors and crashes are in a project of its own, `davy` unless a person took that slug, and `project_status` names it under `davy.project`: `search_logs` and `log_patterns` with that project read its lines, the component that wrote one is in `attributes.component`, and a crash is an issue of level fatal there that `issue_context` opens with its Rust backtrace.
- `mark_fixed` resolves an issue in a release. It reopens when the error comes back in that release or a newer one.
- `set_retention` makes a project keep its logs or its errors longer, from 1 to 36,500 days, or `"default"` for the server's: 30 days of logs and 100 years of errors unless `davy serve` says otherwise. A shorter retention deletes data for good, so the tool refuses it, and a person shortens it with `davy project retention` or in the web UI. The tool also refuses `"default"` for a project with a retention of its own, because a lower default later would delete what the project was set to keep. An agent's prompt can carry text from someone else's error or log line, and that text must not be able to delete data. An issue whose last event expired is deleted, and the error is a new issue if it returns. `project_status` shows what a project keeps.
- `usage` returns, per project and signal, the records and bytes received over a window, and when the disk fills: the median daily growth of all logs together over the last 7 days, the most retention lets Davy take, and the day the disk reaches 90% of its size. Its `what_if` recomputes the forecast for another log retention, or another daily log volume of all projects together, without changing anything, so the agent can say what a shorter retention would buy before a person sets it.
- `submit_feedback` reports a tool that was missing, returned the wrong thing or needed a workaround. `davy feedback --data /var/lib/davy` lists the reports. Davy sends them nowhere unless a channel was added with `--feedback`.

## Connect a new app

On the Davy host, run this as the service user (add `--local` only for an
app running on the same host):

<!-- e2e: skip, e2e/run.sh exercises connect with its own data directory -->
```sh
davy connect my-app --data /var/lib/davy --sdk node
```

The command creates or reuses the project and prints JSON. Without `--sdk`,
it lists supported SDK names. `davy project list --data /var/lib/davy` lists
existing project IDs and slugs. Both commands require local data access.

1. Read `project.dsn`, install `sdk.package` using `sdk.install`, and add
   `sdk.init` at app startup. The init snippets assume the usual SDK imports;
   follow the platform page for a complete example.
2. Put the DSN in `sdk.dsn_location`, never in a committed file. `.env` files
   need a loader or launcher that exports their variables. For systemd, put
   `Environment="SENTRY_DSN=..."` in a service drop-in with `systemctl edit`,
   then restart the app. Browser build variables are public in the bundle.
3. Run the returned `test_event` command from the host that can reach its DSN.
   This checks Davy ingest independently of the SDK.
4. Poll MCP `project_status` with `{"slug":"my-app"}` until `event_count`
   increases and `last_event_time` advances. Record the baseline first if
   the project already has events. Then trigger an SDK error and check again
   to verify the app's SDK and network path too.

MCP `connect_project` takes `slug` and optional `sdk` and returns the same
JSON for an existing project, using the running server's public URL. It
cannot create a project: the admin MCP token keeps its existing authority,
so new keys require the local CLI. CLI `--local` reads the listener from
`setup.json`, uses its port and loopback address (IPv4 or IPv6), and uses
HTTP without going through the TLS proxy. If `serve` overrides the saved
listener, save that address with `setup` first. A phone's loopback is the
phone, so Android needs Davy's reachable public URL, VPN or Tailscale.

`project_status` returns `event_count`, `issue_count`, `last_event_time`,
`last_log_time` and `retention`. Times are RFC 3339 and null before the first record. Log visibility waits
for the log writer's commit. Its `davy` block, the same for every slug, is what Davy reported of itself in the
last 24 hours: its own warnings and errors, its latest crash issues and crash files not yet recorded.

Platform guides: [Node](node.md), [browser](browser.md), [Python](python.md),
[Go](go.md), [Android](android.md), [Rust](rust.md), [.NET](dotnet.md).
