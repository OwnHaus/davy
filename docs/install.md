# Install Davy

These steps install Davy on a Linux server with systemd, or on a Mac for one person. A coding agent can follow them from top to bottom. Each step has the command, what it prints when it works, and what to do when it does not.

Run every command as a user who may use `sudo`. Davy itself runs as its own user and never as root.

## Before you start

Ask the person two things and nothing else:

1. **The public URL**, the address apps and agents will reach Davy at, such as `https://davy.example.com`. Ask whether a TLS proxy (Caddy, nginx, a load balancer) already serves that name. On a laptop for one person, use `http://127.0.0.1:8000`.
2. **Optional: an SMTP server** for alert e-mail, as a URL such as `smtps://user:password@smtp.example.com:465`, and the sender address. Without it, alerts go to webhooks and Slack.

In the commands below, replace `https://davy.example.com` with the public URL. Keep `/var/lib/davy` as the data directory unless the person names another one. On a Mac for one person, leave out `sudo runuser -u davy --` and use the data directory from step 4.

## 1. Get the binary

Pick the file for the machine: `davy-x86_64-unknown-linux-musl`, `davy-aarch64-unknown-linux-musl`, `davy-aarch64-apple-darwin` or `davy-x86_64-apple-darwin`. Download it with the checksums and check it before you run it:

<!-- e2e: skip, the test container already has the binary -->
```sh
FILE=davy-x86_64-unknown-linux-musl
curl -fsSLO "https://github.com/dominikzabcik/davy/releases/latest/download/$FILE"
curl -fsSLO "https://github.com/dominikzabcik/davy/releases/latest/download/SHA256SUMS"
grep " $FILE\$" SHA256SUMS | sha256sum -c -
```

It prints `davy-x86_64-unknown-linux-musl: OK`. On a Mac, use `shasum -a 256 -c -` in place of `sha256sum -c -`.

When `gh` is installed, also check that GitHub built the file from the Davy repository:

<!-- e2e: skip, the test container already has the binary -->
```sh
gh attestation verify "$FILE" --repo dominikzabcik/davy
```

If either check fails, stop and tell the person. Do not run the file.

Until the first release is out, build from source instead (needs Rust): `git clone https://github.com/dominikzabcik/davy && cd davy && cargo build --release`, then use `target/release/davy` as the file.

## 2. Put it on the path

<!-- e2e: skip, the test container already has the binary -->
```sh
sudo install -m 755 "$FILE" /usr/local/bin/davy
davy --version
```

It prints `davy` and the version.

## 3. Create the davy user and the data directory

On a Mac for one person, skip this step. Davy runs as that person and keeps its data in their home directory (step 4).

```sh
sudo useradd --system --home-dir /var/lib/davy --shell /usr/sbin/nologin davy
sudo install -d -o davy -g davy -m 700 /var/lib/davy
```

Both print nothing. If `useradd` says the user already exists, go on.

## 4. Run setup

```sh
sudo runuser -u davy -- davy setup --data /var/lib/davy --public-url https://davy.example.com --service systemd
```

It prints JSON with the files it wrote and the commands to run next. It writes a random admin token to `/var/lib/davy/token`, the settings to `/var/lib/davy/setup.json` and a systemd unit to `/var/lib/davy/davy.service`. Running it again keeps the token.

On a Mac for one person, run it as that person:

<!-- e2e: skip, the test runs Linux -->
```sh
davy setup --data "$HOME/.local/share/davy" --public-url http://127.0.0.1:8000 --service launchd
```

When it fails, the error says what to change. Run setup again after the change.

## 5. Optional: e-mail alerts

Skip this step when the person gave no SMTP server. The SMTP URL holds a password, so it goes into a file only the davy user can read, never on a command line. Write the URL the person gave between the two `EOF` lines:

<!-- e2e: skip, the test has no SMTP server -->
```sh
sudo runuser -u davy -- sh -c 'umask 077 && cat > /var/lib/davy/smtp-url' <<'EOF'
smtps://user:password@smtp.example.com:465
EOF
sudo runuser -u davy -- davy setup --data /var/lib/davy --public-url https://davy.example.com --service systemd --mail-from 'Davy <alerts@example.com>'
```

Setup prints the same JSON as before. Use `smtps://` for port 465 and `smtp://` with port 587 for STARTTLS.

## 6. Start the service

Run the commands in `next` from the JSON of step 4, in order. On Linux they are:

```sh
sudo install -m 644 /var/lib/davy/davy.service /etc/systemd/system/davy.service
sudo systemctl daemon-reload
sudo systemctl enable davy
sudo systemctl restart davy
```

`systemctl enable` prints that it created a symlink. The others print nothing. If the service does not start, `sudo journalctl -u davy -n 50` shows why.

## 7. TLS

Skip this step when the public URL starts with `http://127.0.0.1`, or when a TLS proxy already serves the name and forwards to `127.0.0.1:8000`.

Otherwise install Caddy, which gets a certificate on its own. The name must already point at this server in DNS, and ports 80 and 443 must be open.

<!-- e2e: skip, the test has no public name -->
```sh
sudo apt-get install -y caddy
sudo tee /etc/caddy/Caddyfile > /dev/null <<'EOF'
davy.example.com {
	reverse_proxy 127.0.0.1:8000
}
EOF
sudo systemctl reload caddy
```

Replace `davy.example.com` with the name from the public URL, without `https://`.

## 8. Check the install

```sh
sudo runuser -u davy -- davy doctor --data /var/lib/davy
```

Every line starts with `ok`. A line that starts with `fail` or `warn` has a `fix:` line under it. Run the fix, then run doctor again until no line fails. `--json` prints the same checks as JSON.

## 9. Connect the app

Create a project for the app. It prints the DSN, the address the app's Sentry SDK sends errors to:

```sh
sudo runuser -u davy -- davy project create my-app --data /var/lib/davy
```

It prints a URL such as `https://3f9c...@davy.example.com/1`. Running it again for the same name prints the same DSN.

Then follow the page for the app's language. Each uses the official SDK and changes only the DSN or token:

- [Node](connect/node.md)
- [Browser JavaScript with source maps](connect/browser.md)
- [Python](connect/python.md)
- [Go](connect/go.md)
- [Logs from pino and other Logtail shippers](connect/logtail.md)
- [OpenTelemetry](connect/opentelemetry.md)

## 10. Connect the coding agent

Follow [connect/agents.md](connect/agents.md). The agent then reads errors and logs over MCP and marks issues fixed.

## Updating

Repeat steps 1 and 2 with the new version, then `sudo systemctl restart davy` and step 8. The data directory and the token stay.
