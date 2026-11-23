# Connect a coding agent

Davy serves MCP at `/mcp` of the public URL. Claude Code, Codex and Cursor connect with the admin token and get seven tools: `schema`, `sql`, `search_logs`, `count_logs`, `issue_context`, `mark_fixed` and `submit_feedback`.

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
- `sql` runs one read-only query, and `schema` describes the tables. The `client_reports` table says how many errors and logs each SDK dropped before sending, and why, so the agent can tell a quiet app from one whose `beforeSend` drops everything.
- `mark_fixed` resolves an issue in a release. It reopens when the error comes back in that release or a newer one.
- `submit_feedback` reports a tool that was missing, returned the wrong thing or needed a workaround. `davy feedback --data /var/lib/davy` lists the reports. Davy sends them nowhere unless a channel was added with `--feedback`.
