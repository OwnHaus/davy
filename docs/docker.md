# Run Davy in Docker

The image `ghcr.io/ownhaus/davy` holds the static binary on a distroless base and runs as a non-root user. It is built for amd64 and arm64 and signed. Tags are the version, such as `0.1.0`, and `latest` for the newest release. Pin the version on a server, so an upgrade happens when you choose.

The image has no shell. Every command below runs the `davy` binary, which sits at `/davy`, and keeps the data in the volume at `/data`.

## Start it

```sh
docker volume create davy
docker run --rm -v davy:/data ghcr.io/ownhaus/davy:0.1.0 \
  setup --data /data --public-url http://127.0.0.1:8000 --listen 0.0.0.0:8000
docker run -d --name davy --restart unless-stopped \
  -p 127.0.0.1:8000:8000 -v davy:/data ghcr.io/ownhaus/davy:0.1.0 serve --data /data
```

`setup` writes the settings, a random admin token and the address to listen on into the volume. `serve` reads them from there. `--listen 0.0.0.0:8000` is the address inside the container. `-p 127.0.0.1:8000:8000` keeps the port on this machine only.

## Add a project and a person

```sh
docker exec davy /davy project create my-app --data /data
docker exec davy /davy user add you@example.com --data /data
```

The first prints the DSN for `Sentry.init({ dsn })`. Its public key is also the token for log shippers, see [Logtail](connect/logtail.md) and [OpenTelemetry](connect/opentelemetry.md). The second prints a link, valid once for 24 hours, where the person sets a password for the UI at `/ui/`.

## Behind TLS

Apps on other machines need a public URL with TLS. Run `setup` again with that URL, then restart:

```sh
docker run --rm -v davy:/data ghcr.io/ownhaus/davy:0.1.0 \
  setup --data /data --public-url https://davy.example.com --listen 0.0.0.0:8000
docker restart davy
```

Point the proxy (Caddy, nginx, a load balancer) at `127.0.0.1:8000`. With Caddy the whole config is:

```text
davy.example.com {
	reverse_proxy 127.0.0.1:8000
}
```

Leave the port bound to `127.0.0.1`. Davy leaves slow and idle connections to the proxy, so it should not face the internet on its own.

## Check it

```sh
docker exec davy /davy doctor --data /data
```

It checks the data directory, the token, TLS, that the server answers at the public URL, and free disk. Each failed check prints the fix.

## Connect a coding agent

The admin token turns on MCP at `/mcp`. Copy it out of the volume and print the config for the agent:

```sh
docker cp davy:/data/token ./davy-token
export DAVY_TOKEN="$(cat ./davy-token)"
docker exec davy /davy mcp-config --client claude-code --data /data
```

Use `codex` or `cursor` for those clients. The config reads the token from `DAVY_TOKEN`, so the file holds no secret. Keep `davy-token` out of the repository. [Coding agents](connect/agents.md) has the rest.

## Back up and upgrade

`davy backup` writes the whole volume to one file while the container runs: the database, the logs, the source maps, the settings and the admin token. The server stops writing logs for under a second on a fast disk, a few seconds on a slow or busy one. With `-` it writes to stdout, so the file lands on the host:

```sh
(umask 077 && docker exec davy /davy backup --data /data - > "davy-$(date +%F).tar.zst")
```

It prints what it backed up on stderr. The file holds the server's secrets, such as the admin token, the SMTP URL, alert channel URLs, password hashes and sign-in sessions, so keep it where only you can read it, as the `umask` does, and copy it off the host. Davy 0.2.0 and older cannot pause for a backup: for those, stop the container and back up with the new image as in the restore below, `docker run --rm -v davy:/data ghcr.io/ownhaus/davy:VERSION backup --data /data - > FILE`, with `VERSION` the release you upgrade to. [Backups](install.md#backups) in the install guide has a daily timer, an off-site copy and a restore drill; in a timer use the `docker exec` line above. `docker exec -i davy /davy backup verify - < FILE` checks a backup without restoring it.

To restore, stop the container and restore into the volume. `--replace` moves what the volume held into `/data/.replaced-TIME-PID`; into a new, empty volume leave it out:

```sh
docker stop davy
docker run --rm -i -v davy:/data ghcr.io/ownhaus/davy:VERSION restore --data /data --replace - < davy-2026-11-28.tar.zst
docker start davy
```

`VERSION` is the image the container runs, at least as new as the Davy that took the backup and no older than the first release with `davy restore`. A backup from an older Davy is upgraded when the server starts.

To upgrade, back up first, then start the new version on the same volume:

```sh
docker pull ghcr.io/ownhaus/davy:0.2.0
docker stop davy
docker rm davy
docker run -d --name davy --restart unless-stopped \
  -p 127.0.0.1:8000:8000 -v davy:/data ghcr.io/ownhaus/davy:0.2.0 serve --data /data
```

A newer Davy migrates the data of an older one. It waits up to 10 s for other processes to close the data and then refuses to migrate, so stop the old container before the new one starts, and never run two versions on one volume at once. For the same reason a rolling update that starts the new container before it stops the old one cannot finish: stop first, as Kubernetes `Recreate` does. An older one refuses data that a newer one migrated and says so, so a downgrade means restoring the backup taken before the upgrade, without what arrived since. The [changelog](../CHANGELOG.md) says when a release migrates data.

An upgrade that migrates runs in one transaction before the server answers, so ingest waits until it is done. The release that adds retention per project builds two indexes over every stored event: a database with tens of millions of events waits about a minute or more. Each index takes about 20 bytes per stored event, and SQLite writes it to its write-ahead log first, so leave free disk of about 80 bytes per stored event, next to the backup.

## Verify the image

```sh
cosign verify ghcr.io/ownhaus/davy:0.1.0 \
  --certificate-identity-regexp '^https://github.com/dominikzabcik/davy/.github/workflows/release.yml@refs/tags/v0.1.0$' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
```

It prints the checks it passed. Davy 0.1.0 was built before the repository moved to `OwnHaus/davy`, so its signature names `dominikzabcik/davy`, as above. From the next release the identity is `^https://github.com/OwnHaus/davy/.github/workflows/release.yml@refs/tags/vX.Y.Z$`. [RELEASING.md](../RELEASING.md) lists every file a release ships and how each is checked.

## Not covered here yet

E-mail alerts from the container. Webhooks and Slack work the same as outside Docker, see `davy channel add --help`.
