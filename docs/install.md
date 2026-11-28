# Install Davy

These steps install Davy on a Linux server with systemd, or on a Mac for one person. A coding agent can follow them from top to bottom. Each step has the command, what it prints when it works, and what to do when it does not.

Run every command as a user who may use `sudo`. Davy itself runs as its own user and never as root.

## Before you start

Ask the person two things and nothing else:

1. **The public URL**, the address apps and agents will reach Davy at, such as `https://davy.example.com`. Ask whether a TLS proxy (Caddy, nginx, a load balancer) already serves that name. On a laptop for one person, use `http://127.0.0.1:8000`.
2. **Optional: an SMTP server** for alert e-mail, its host and port, and the sender address. Do not ask for the SMTP password. The person types it in step 5, so it never passes through you. Without SMTP, alerts go to webhooks and Slack.

In the commands below, replace `https://davy.example.com` with the public URL. Keep `/var/lib/davy` as the data directory unless the person names another one. On a Mac for one person, leave out `sudo runuser -u davy --` and use the data directory from step 4.

## 1. Get the binary

Pick the file for the machine: `davy-x86_64-unknown-linux-musl`, `davy-aarch64-unknown-linux-musl`, `davy-aarch64-apple-darwin` or `davy-x86_64-apple-darwin`. Download it with the checksums and check it before you run it:

<!-- e2e: skip, the test container already has the binary -->
```sh
FILE=davy-x86_64-unknown-linux-musl
curl -fsSLO "https://github.com/OwnHaus/davy/releases/latest/download/$FILE"
curl -fsSLO "https://github.com/OwnHaus/davy/releases/latest/download/SHA256SUMS"
grep " $FILE\$" SHA256SUMS | sha256sum -c -
```

It prints `davy-x86_64-unknown-linux-musl: OK`. On a Mac, use `shasum -a 256 -c -` in place of `sha256sum -c -`.

When `gh` is installed, also check that GitHub built the file from the Davy repository:

<!-- e2e: skip, the test container already has the binary -->
```sh
gh attestation verify "$FILE" --repo OwnHaus/davy --format json | grep -c '"verificationResult"'
```

It prints `1` or more, one per attestation that passed. Without `--format json`, `gh` prints the result only in an interactive terminal. Run by an agent, it prints nothing and exits 0 when the check passed.

Davy 0.1.0 was built before the repository moved to `OwnHaus/davy`, and GitHub keeps its attestations under the old owner. For 0.1.0, `gh` answers `HTTP 404`: run the same command with `--owner dominikzabcik` in place of `--repo OwnHaus/davy`.

If either check fails, stop and tell the person. Do not run the file.

For a machine without a release build, build from source instead (needs Rust, Node and pnpm): `git clone https://github.com/OwnHaus/davy && cd davy && scripts/ui && cargo build --release`, then use `target/release/davy` as the file.

## 2. Put it on the path

<!-- e2e: skip, the test container already has the binary -->
```sh
sudo install -m 755 "$FILE" /usr/local/bin/davy
davy --version
```

It prints `davy` and the version.

On a Mac for one person, install it into their home directory instead, without `sudo`:

<!-- e2e: skip, the test runs Linux -->
```sh
mkdir -p "$HOME/.local/bin"
install -m 755 "$FILE" "$HOME/.local/bin/davy"
"$HOME/.local/bin/davy" --version
```

If `davy --version` then says `command not found`, `~/.local/bin` is not on the path. Add `export PATH="$HOME/.local/bin:$PATH"` to `~/.zprofile` and open a new terminal, or call Davy by its full path.

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

It prints JSON with the files it wrote and the commands to run next. It writes a random admin token to `/var/lib/davy/token` and the settings to `/var/lib/davy/setup.json`, which the server reads at start. Running it again keeps the token.

On a Mac for one person, run it as that person:

<!-- e2e: skip, the test runs Linux -->
```sh
davy setup --data "$HOME/.local/share/davy" --public-url http://127.0.0.1:8000 --service launchd
```

When it fails, the error says what to change. Run setup again after the change.

## 5. Optional: e-mail alerts

Skip this step when the person gave no SMTP server. The SMTP URL holds a password, so it goes into a file only the davy user can read, never on a command line, and the person writes it themselves. Ask the person to run this in their own terminal on the server, paste the URL, press Enter and then Ctrl-D:

<!-- e2e: skip, the test has no SMTP server -->
```sh
sudo runuser -u davy -- sh -c 'umask 077 && cat > /var/lib/davy/smtp-url'
```

The URL is `smtps://user:password@smtp.example.com:465` for port 465, or `smtp://user:password@smtp.example.com:587?tls=required` for STARTTLS on port 587. Davy refuses a password without TLS. Characters such as `@`, `:` or `/` in the user or password must be percent-encoded, `@` as `%40`.

When the person says it is done, run setup again with the sender:

<!-- e2e: skip, the test has no SMTP server -->
```sh
sudo runuser -u davy -- davy setup --data /var/lib/davy --public-url https://davy.example.com --service systemd --mail-from 'Davy <alerts@example.com>'
```

Setup prints the same JSON as before. Step 8 checks that the SMTP server accepts the login.

## 6. Start the service

On Linux, run these commands. They match `next` in the JSON of step 4. The first one generates the systemd unit and writes it to `/etc/systemd/system` only when that worked, so a failed run leaves an installed unit as it was. The unit holds only the binary, the data directory and the user, never anything from the data directory, so the davy user cannot change what root installs.

```sh
unit="$(davy systemd-unit --data /var/lib/davy --user davy)" && printf '%s\n' "$unit" | sudo tee /etc/systemd/system/davy.service > /dev/null
sudo systemctl daemon-reload
sudo systemctl enable davy
sudo systemctl restart davy
```

`systemctl enable` prints that it created a symlink. The others print nothing. On a Mac, run the commands in `next` instead. If the service does not start, `sudo journalctl -u davy -n 50` shows why.

## 7. TLS

Skip this step when the public URL starts with `http://127.0.0.1`, or when a TLS proxy already serves the name and forwards to `127.0.0.1:8000`.

Otherwise install Caddy, which gets a certificate on its own. The name must already point at this server in DNS, and ports 80 and 443 must be open. Install it, keep a copy of its config and compare the config with the one the package installed:

<!-- e2e: skip, the test has no public name -->
```sh
sudo apt-get install -y caddy
sudo cp -np /etc/caddy/Caddyfile /etc/caddy/Caddyfile.before-davy
sudo md5sum /etc/caddy/Caddyfile
dpkg-query -W -f='${Conffiles}\n' caddy
```

Do not print the Caddyfile itself. It can hold passwords of the other sites it serves. When the checksum from `md5sum` is the one `dpkg-query` lists for `/etc/caddy/Caddyfile`, the file is still the package's sample, so start Davy's config from an empty file:

<!-- e2e: skip, the test has no public name -->
```sh
sudo install -m 644 /dev/null /etc/caddy/Caddyfile.davy
```

When the checksums differ, the file serves other sites, so start from a copy of it and Davy goes after them:

<!-- e2e: skip, the test has no public name -->
```sh
sudo cp -p /etc/caddy/Caddyfile /etc/caddy/Caddyfile.davy
```

Then add Davy, check the new file and only then put it in place and reload:

<!-- e2e: skip, the test has no public name -->
```sh
sudo tee -a /etc/caddy/Caddyfile.davy > /dev/null <<'EOF'
davy.example.com {
	reverse_proxy 127.0.0.1:8000
}
EOF
sudo caddy validate --config /etc/caddy/Caddyfile.davy --adapter caddyfile && sudo mv /etc/caddy/Caddyfile.davy /etc/caddy/Caddyfile && sudo systemctl reload caddy
```

Replace `davy.example.com` with the name from the public URL, without `https://`. `caddy validate` prints `Valid configuration` at the end. When it prints an error instead, the running config is untouched. Tell the person and show them the error.

## 8. Check the install

```sh
sudo runuser -u davy -- davy doctor --data /var/lib/davy
```

Every line starts with `ok`. A line that starts with `fail` or `warn` has a `fix:` line under it. Run the fix, then run doctor again until no line fails. `--json` prints the same checks as JSON.

Davy records its own warnings, errors and crashes in a project of its own, `davy`, or `davy-self` when a project of yours already has the slug `davy`. The `davy` line of doctor names the slug, warns when Davy crashed in the last 24 hours and names the command that shows the crash. `davy logs --data /var/lib/davy --project davy --level warn` lists Davy's own warnings of the last day. `davy serve --no-self-report` turns this off.

Then give the person the web UI, before the steps below, so they can watch them happen. Ask for their e-mail and create their user:

```sh
sudo runuser -u davy -- davy user add person@example.com --data /var/lib/davy
```

It prints a link such as `https://davy.example.com/ui/welcome#...`. The link works once and for 24 hours. Give it to the person. They set a password there and land on the setup page, which shows each of the next steps as done when Davy sees it happen. The command refuses a plain `http` public URL to another machine, because the browser would not keep the login. Run step 7 first in that case.

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

## Backups

`davy backup` writes the whole data directory to one file: the database, the logs, the source maps, the settings and the admin token. The server keeps running and taking requests; it stops writing logs for under a second on a fast disk, a few seconds on a slow or busy one, while the backup takes its snapshot. With no server running it works the same.

A backup holds the server's secrets: the admin token, the SMTP URL, alert channel URLs, password hashes and sign-in sessions. Anyone who reads the file can act as the server's admin, so keep it where only you can read it. Davy writes it for its owner only.

```sh
sudo install -d -o davy -g davy -m 700 /var/backups/davy
sudo runuser -u davy -- davy backup --data /var/lib/davy "/var/backups/davy/davy-$(date +%F).tar.zst"
```

It prints how many files and bytes it backed up, how long the log writer paused, and that the file holds secrets. Run it as the user that owns the data directory, or it says which user to run it as, and write the file outside the data directory. `davy backup --data /var/lib/davy -` writes to stdout instead, to pipe it somewhere. Davy 0.2.0 and older cannot pause for a backup: while one of those runs, `davy backup` refuses and says to stop it first.

### Every day

A systemd timer takes a backup every night and keeps two weeks of them:

```sh
sudo tee /etc/systemd/system/davy-backup.service > /dev/null <<'EOF'
[Unit]
Description=Back up Davy

[Service]
Type=oneshot
User=davy
UMask=0077
ExecStart=/bin/sh -c 'davy backup --data /var/lib/davy /var/backups/davy/davy-$$(date +%%F).tar.zst && find /var/backups/davy -name "davy-*.tar.zst" -mtime +14 -delete'
EOF
sudo tee /etc/systemd/system/davy-backup.timer > /dev/null <<'EOF'
[Unit]
Description=Back up Davy every night

[Timer]
OnCalendar=*-*-* 03:00
RandomizedDelaySec=1h
Persistent=true

[Install]
WantedBy=timers.target
EOF
sudo systemctl daemon-reload
sudo systemctl enable --now davy-backup.timer
sudo systemctl start davy-backup.service
```

The last command takes a backup now and returns when it is done; `sudo journalctl -u davy-backup -n 20` shows what it printed. `davy doctor` warns when the last backup is 7 days old or more, so a timer that stopped working shows there.

Without systemd, a line in `/etc/cron.d/davy-backup` does the same:

<!-- e2e: skip, the test uses the systemd timer -->
```sh
echo '0 3 * * * davy davy backup --data /var/lib/davy /var/backups/davy/davy-$(date +\%F).tar.zst > /dev/null && find /var/backups/davy -name "davy-*.tar.zst" -mtime +14 -delete' | sudo tee /etc/cron.d/davy-backup > /dev/null
```

On a Mac for one person, run `crontab -e` and add a line such as `0 3 * * * $HOME/.local/bin/davy backup --data $HOME/.local/share/davy $HOME/Backups/davy-$(date +\%F).tar.zst > /dev/null`.

### Off the host

A backup on the server's disk is lost with the disk. Copy it to another machine or a bucket after each run, for example with `rsync` over SSH, or with `rclone`:

<!-- e2e: skip, the test has no other host -->
```sh
sudo rsync -a /var/backups/davy/ backup@backup.example.com:davy/
```

Wherever it goes, only you should read it. To encrypt it on the way, pipe the stream through `age` and keep the key off this server:

<!-- e2e: skip, the test has no age key -->
```sh
sudo runuser -u davy -- davy backup --data /var/lib/davy - | age -r age1yourpublickey > "davy-$(date +%F).tar.zst.age"
```

### A restore drill

A backup is only as good as its restore. Once a month, check the newest backup and restore it next to the server, which keeps running:

```sh
latest="/var/backups/davy/$(sudo ls -1 /var/backups/davy | grep '^davy-.*\.tar\.zst$' | tail -n 1)"
sudo runuser -u davy -- davy backup verify "$latest"
sudo runuser -u davy -- davy restore --data /var/tmp/davy-drill "$latest"
sudo runuser -u davy -- davy issues --data /var/tmp/davy-drill
sudo rm -r /var/tmp/davy-drill
```

`verify` reads the whole file, checks every file in it against its checksum and prints which Davy took it. `restore` checks the same before it puts anything in place, and `davy issues` lists the issues the backup holds.

### Restore

On the same host, stop the server, restore over its data, and start it again. `--replace` moves what the data directory held into `/var/lib/davy/.replaced-TIME-PID`, which you delete once the server runs well:

<!-- e2e: skip, the test keeps its data -->
```sh
sudo systemctl stop davy
sudo runuser -u davy -- davy restore --data /var/lib/davy --replace /var/backups/davy/davy-2026-11-28.tar.zst
sudo systemctl start davy
```

On a new host, follow steps 1 to 3, copy the backup over, run the same `davy restore` without `--replace` into the empty `/var/lib/davy`, skip steps 4 and 5, and go on with step 6. The token and the settings come from the backup. Restore refuses a directory a server runs on, and one that holds anything unless you pass `--replace`. It refuses a backup from a newer Davy than itself; one from an older Davy is upgraded when `davy serve` starts, as after an update. The server does not start while a restore runs. A restore stopped after it checked the backup, by a crash or a full disk, is finished by the next `davy restore` or `davy serve` on the directory, which says so.

## Updating

Repeat steps 1 and 2 with the new version, then `sudo systemctl restart davy` and step 8. On a Mac, restart with `launchctl kickstart -k "gui/$(id -u)/davy"`. The data directory and the token stay.

A release that migrates data says so in the [changelog](../CHANGELOG.md). Under systemd the server reports it is ready only once the migration is done, and systemd stops a start that takes longer than 90 s and starts it again from the beginning. Before such an upgrade of a large database, run `sudo systemctl edit davy` and add `TimeoutStartSec=15min` under `[Service]`.

A new version that changes the schema migrates the data the first time it opens it, and only while no other process has the database open. Restart the service before you run any other `davy` command with the new binary. Until then such a command waits 10 s, stops with an error that another process has the database open, and changes nothing. `sudo lsof /var/lib/davy/davy.sqlite` lists what holds it. Take a backup first, as in [Backups](#backups): an older Davy refuses migrated data, so a downgrade means restoring that backup with the new binary's `davy restore --replace`, then starting the old version. To back up before an update from 0.2.0 or older, which cannot pause for a backup, run `sudo systemctl stop davy` after step 2, then the backup with the new binary, then the restart.
