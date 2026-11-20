# Security

## Reporting a vulnerability

Report it privately through GitHub's "Report a vulnerability" button on this repository. Do not open a public issue. You get an answer within 7 days. A fix ships before the details are public.

## What Davy protects

Davy stores error events, logs and source maps. They can hold stack traces, request data and personal data that the sending app put there. Davy assumes its operator controls the machine and the data directory. It defends the network side.

## Threat model

Every byte that arrives over the network is hostile until a parser accepts it. Davy defends against:

- **Anyone on the internet with an ingest URL.** Ingest keys are public by design, because browser SDKs ship them. A key lets a sender write events and logs into one project and nothing else. Davy caps every body before and after decompression, answers `429` when a writer queue is full and never buffers without a bound.
- **Anyone without the admin token.** `/mcp` and the source map upload need `Authorization: Bearer <token>`. The server keeps only a hash of the token and compares in constant time. Without a token, `/mcp` and the upload routes are not mounted at all.
- **A browser page that points at a local Davy.** `/mcp` accepts only known `Host` headers, which stops DNS rebinding.
- **An agent with the admin token that runs arbitrary SQL.** `query.rs` opens the database read-only behind an SQLite authorizer, and limits rows, text size and run time.
- **Uploaded files that name paths.** Chunks and builds are stored under names Davy computes (a SHA-1 or a debug ID). Nothing from a request becomes a path.

Out of scope:

- An attacker with a shell on the host or write access to the data directory.
- Denial of service by a sender with a valid key that stays under the caps. Rate limits per key are planned for the cloud service.
- Secrets that the sending app puts into events. Scrubbing belongs in the SDK's `beforeSend`.

## Running it safely

- Put Davy behind TLS. It speaks plain HTTP.
- Generate the admin token with at least 32 random bytes, for example `openssl rand -hex 32`, and pass it through `DAVY_TOKEN`, not the command line.
- Keep the data directory readable only by the user that runs Davy.
