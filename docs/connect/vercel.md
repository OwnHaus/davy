# Send logs from Vercel with a Log Drain

A Vercel Log Drain sends the logs of your Vercel projects to a URL you give it. Davy takes them at `/vercel/logs` and stores each one as a log you can search with `davy logs`, the UI and the MCP tool `search_logs`.

The endpoint follows Vercel's documented [log schema](https://vercel.com/docs/drains/reference/logs) and [signing](https://vercel.com/docs/drains/security). It has not yet been checked against a live drain. If Vercel's test or a delivery fails, the status Davy answered says why: `401` or `403` for the key or the signature, `400` for a body that is not JSON or NDJSON.

Drains need a Vercel team on the Pro or Enterprise plan ([Using Drains](https://vercel.com/docs/drains/using-drains)).

You need:

- **The public URL** of Davy, such as `https://davy.example.com`. Vercel must reach it over HTTPS from the internet.
- **A Davy project of its own for the drain**, and its project key: the part of the DSN between `://` and `@`. `davy project create vercel-logs --data /var/lib/davy` prints the DSN, for a new project or one that exists. Use a project that no app's DSN points at. An app ships its DSN, and the key in it writes logs to the project through Davy's other log routes, signed or not (see [what the secret covers](#what-the-secret-covers)). A key that lives only in the drain's header is not public.

The commands below use `vercel-logs` as the project.

## Add the drain

In the Vercel dashboard, open your team's settings, then **Drains**, click **Add Drain** and choose **Logs**.

1. Name the drain and pick the projects whose logs it sends. One Davy project can take several Vercel projects: each log names its Vercel project in `service`.
2. Pick the sources. Each sets `attributes.source` to its value, which Vercel's reference describes as:
   - `lambda`: output from Vercel Functions, such as API routes.
   - `edge`: output from Vercel Functions using the Edge runtime.
   - `build`: output from the build step.
   - `static`: requests to static assets, such as HTML and CSS files.
   - `external`: external rewrites to a different domain.
   - `redirect`: requests redirected by redirect rules.
   - `firewall`: requests denied by Vercel Firewall rules.
3. Pick the environments, and add sampling rules if you want only a share of the logs.
4. For the destination, choose **Custom Endpoint** and fill in:
   - **Endpoint URL**: `https://davy.example.com/vercel/logs`
   - **Format**: JSON or NDJSON. Davy reads both.
   - **Custom Headers**: `Authorization: Bearer <project key>`
   - **Signature Verification Secret**: Vercel fills in a generated one. Keep it or enter your own, and copy it. Do not create the drain yet.
5. Give Davy the secret ([next section](#check-the-signature)), then click **Create Drain**.

Vercel tests the endpoint when you create the drain. Its docs do not say whether that test is signed. This order assumes it is, like every delivery, so no request is ever taken without a signature. If the test fails with `403`, the assumption was wrong: run the command with `--no-secret` instead of `--secret-stdin`, create the drain, and set the secret again right after. Deliveries between those two steps are taken with the key alone.

## Check the signature

Vercel signs each request with the drain's secret in the `x-vercel-signature` header: the hex HMAC-SHA1 of the body exactly as sent. Once the project has the secret, a request to `/vercel/logs` for it without a matching signature is refused with `403`:

<!-- e2e: skip, e2e/run.sh sets a drain secret from stdin -->
```sh
printf '%s' "$VERCEL_DRAIN_SECRET" | sudo runuser -u davy -- davy project vercel vercel-logs --data /var/lib/davy --secret-stdin
```

The secret is read from standard input, never from the command line, without the spaces and line break around it, and is never printed or logged. Davy stores it as it is in its database, since checking a signature needs it, so a copy of the data directory holds it. `davy project vercel vercel-logs --data /var/lib/davy` shows whether a signature is required, and `--no-secret` turns the check off again. A change applies to the next request, with no restart.

### What the secret covers

The secret guards `/vercel/logs` and nothing else. The same project key still writes logs through the Logtail route `/` and the OTLP route `/v1/logs`, and errors and logs through the Sentry envelope routes, with no signature. So the secret proves that a log with `source` `vercel` came in a request signed with the drain's secret, not that every log in the project did. The signature covers the body and no time, so a signed request someone captured can be sent again. That is why the drain gets a project of its own, whose key is only in the drain's header.

Without a secret, the project key alone lets a drain request in, as for every other log source.

## Change the secret

Davy keeps one secret per project, so while Vercel and Davy hold different secrets every delivery gets `403`. Vercel lets you change the secret with **Edit** in the Drains list. Pick one of two orders:

- **No delivery refused, a short unsigned window.** Run `davy project vercel vercel-logs --data /var/lib/davy --no-secret`, change the secret in Vercel, then set the new one with `--secret-stdin`.
- **Nothing unsigned, a short gap.** Change the secret in Vercel and set the same one in Davy right after. Deliveries in between get `403`, and Vercel's docs do not say whether it sends a refused batch again, so count those logs as lost.

## What a log holds

| Vercel field | In Davy |
|---|---|
| `timestamp` | the log's time |
| `level` (`info`, `warning`, `error`, `fatal`) | the level, with `warning` as `warn` |
| `message` | the message |
| `projectName`, else `projectId` | `service` |
| `traceId`, `spanId` | `trace_id`, `span_id` |
| everything else | an attribute under the same name |

So the [fields of Vercel's log schema](https://vercel.com/docs/drains/reference/logs) are what you search for:

<!-- e2e: skip, e2e/run.sh searches a Vercel log -->
```sh
sudo runuser -u davy -- davy logs --data /var/lib/davy --project vercel-logs --since 1h \
  'attributes.source:lambda AND attributes.statusCode:500'
```

`proxy` stays one object, so its fields are `attributes.proxy.path`, `attributes.proxy.statusCode` and so on. `requestId` and `deploymentId` find every log of one request or one deployment.

Vercel's `id` stays an attribute too. Davy drops no duplicates, for any log source, so a batch Vercel sends again, for example after a timeout, is stored twice.

## Personal data

Davy scrubs what it stores like every other log ([data scrubbing](../design.md#server-side-data-scrubbing)). With the default policy, the client's address in `proxy.clientIp` is stored as `[Filtered: ip]`, and sensitive query parameters in `proxy.path`, such as `token=` or `password=`, are filtered. The default policy keeps the rest of what a drain sends, and some of it can be personal data:

- `ja3Digest` and `ja4Digest`, the TLS fingerprints of the visitor's client.
- `proxy.userAgent` and `proxy.referer`.
- The other query parameters in `proxy.path`, such as `?email=...`, since e-mail scrubbing is off by default.

The project's scrub settings filter more: `email` filters e-mail addresses, and `extra_keys` filters fields by name, at any depth. `--settings` replaces the whole policy, and fields left out take their defaults, not their current values:

<!-- e2e: skip, a scrub policy example, e2e covers the default policy -->
```sh
sudo runuser -u davy -- davy project scrub vercel-logs --data /var/lib/davy \
  --settings '{"email":true,"extra_keys":["ja3Digest","ja4Digest","userAgent","referer"]}'
```

To keep addresses out of the drain altogether, open the team's **Settings** in Vercel, then **Security & Privacy**, and turn off **IP Address Visibility**. It hides IP addresses in every drain of the team, and only those.

Logs are kept 30 days unless `davy serve` gets `--log-retention-days` or the project has its own, set with `davy project retention`.
