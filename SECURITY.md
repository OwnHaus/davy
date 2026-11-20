# Security

## Reporting a vulnerability

Report it privately through GitHub's "Report a vulnerability" button on this repository. Do not open a public issue.

- You get a first answer within 3 working days and an assessment within 7.
- A critical or high finding is fixed in a patch release within 30 days of the assessment, others in the next release.
- The fix ships before the details are public. Details go public with the fix, at the latest 90 days after the report, as a GitHub security advisory with a CVE. The reporter is credited unless they ask not to be.

## Supported versions

Before 1.0 only the latest release gets security fixes. See `RELEASING.md`.

## What Davy protects

Davy stores error events, logs and source maps. They can hold stack traces, request data and personal data that the sending app put there. Davy assumes its operator controls the machine and the data directory. It defends the network side.

## Threat model

Every byte that arrives over the network is hostile until a parser accepts it. Davy defends against:

- **Anyone on the internet with an ingest URL.** Ingest keys are public by design, because browser SDKs ship them. A key lets a sender write events and logs into one project and nothing else. Davy caps every body before and after decompression, prices what a body parses into before it parses it, pays for that from one memory budget, answers `429` when the budget or a writer queue is full and never buffers without a bound.
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

## Security program

What runs when. Items marked planned have a milestone in `docs/design.md`.

**On every commit.** `scripts/check` denies `unwrap`, `panic` and silent error drops through clippy, and runs `cargo deny` for RustSec advisories, licenses and crate sources. Each route has a test that a wrong or missing credential is refused, and each cap has a test that a request over it is refused. A change to a trust boundary gets a security read by a second model (`CONTRIBUTING.md`).

**On every push.** CI repeats the gate on a clean runner, with every action pinned to a commit SHA, no stored credentials in the checkout, and `zizmor` checking the workflows. `scripts/check-commits` checks each new commit message.

**Every week.** CI runs the gate against the newest advisories, because a new advisory can hit code that did not change. Dependabot proposes updates for crates, e2e packages and actions.

**At the end of each milestone.** A security review of everything since the last one, by a model that did not write the code, against the threat model above. The first one, of `16f413c`, produced the rules in the Hardening section of `docs/design.md`. Each finding becomes a fix with a test, or an entry under "Before the cloud launch".

**Before release 0.1.0 (planned).**

- Fuzz targets with `cargo fuzz` for every parser: the envelope, the event, Logtail JSON and msgpack, OTLP protobuf and JSON, the Sentry log item, artifact bundles and source maps. Each runs for at least an hour before the release, and CI runs each for a few minutes every night. A crash becomes a fixture and a unit test.
- An internal penetration test of a running server, against OWASP ASVS 5.0 level 2 and the OWASP API Security Top 10, with a written report. Open findings stay private until fixed. Fixed ones are summarized under Security in the changelog.
- Signed releases with SBOM and build provenance, as described in `RELEASING.md`.

**Before the cloud takes outside users (planned).**

- Every item under "Before the cloud launch" in `docs/design.md` is closed, and tests show that no key, token or agent tool of one tenant reaches another tenant's data.
- An external penetration test of the cloud by an independent firm, grey box with source access. Its critical and high findings are fixed before the beta opens. It repeats every year and after a change to the tenant model.

**After the public release (planned).**

- Private vulnerability reporting is enabled on the public repository, with CodeQL and the OpenSSF Scorecard in CI.
- The project earns the OpenSSF Best Practices badge, passing first, then silver.
- A bug bounty program opens once the cloud has paying customers.

**EU Cyber Resilience Act.** Selling Davy under a commercial license makes the binary a product with digital elements under the CRA. The duty to report an actively exploited vulnerability to ENISA applies from 11 September 2026. The full duties, among them an SBOM, vulnerability handling and security updates for the support period, apply from 11 December 2027. The release pipeline and this program are built to meet them. A lawyer confirms the scope before the first commercial sale.
