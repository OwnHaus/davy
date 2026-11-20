# Releasing

No version has been released yet. `0.1.0` in `Cargo.toml` is unreleased. The milestone "Release 0.1.0" in `docs/design.md` builds what this file marks as planned. Until then this file is the spec for it.

## Versions

- Davy follows [Semantic Versioning](https://semver.org/). The public surface is the ingest routes, the MCP tools, the CLI commands and their `--json` output, the webhook body and the data directory.
- Before 1.0 a minor release may break the public surface and a patch release never does. 1.0 comes when the data directory and the webhook body are promised stable.
- A newer Davy opens every data directory an older one wrote and migrates it. An older Davy never opens a newer one. A release that migrates data says so in the changelog. Downgrades are not supported, so back up the data directory before an upgrade.
- A release is a tag `vX.Y.Z` on `main`, annotated and signed.

## Cadence

- A release ships when `## Unreleased` holds something an operator wants. There is no calendar.
- A fix for a confirmed vulnerability ships as a patch release within the times in `SECURITY.md`, ahead of anything else.

## Steps

1. `main` is clean, pushed and green in CI.
2. `scripts/release X.Y.Z` (planned) runs the gate and e2e, sets the version in `Cargo.toml` and `Cargo.lock`, turns `## Unreleased` into `## X.Y.Z (YYYY-MM-DD)` with an empty `## Unreleased` above it, commits `Release X.Y.Z` and creates the signed tag `vX.Y.Z`. The tag message is the changelog section.
3. `git push origin main vX.Y.Z`. The tag starts the release workflow.
4. The release workflow (planned) builds and publishes a draft release, below.
5. The owner downloads one artifact, verifies it as a user would (below), runs it against the e2e suite and publishes the draft.

## What a release contains (planned)

- **Binaries** for Linux x86_64 and aarch64 (static, musl) and macOS aarch64 and x86_64, built from the tag on a clean GitHub runner with `--locked`.
- **Embedded dependency list.** The binaries are built with `cargo auditable`, so `cargo audit bin`, Trivy and Grype can scan a binary on a server without its source.
- **SBOM** in CycloneDX format for each binary.
- **Checksums** in `SHA256SUMS`.
- **Build provenance.** GitHub artifact attestations (SLSA build provenance) for every binary and the SBOM, signed through Sigstore. A user checks a download with `gh attestation verify <file> --repo <owner>/davy`.
- **Container image** `ghcr.io/<owner>/davy:X.Y.Z` for amd64 and arm64: the static binary on a distroless base, running as a non-root user, signed with cosign keyless signing, with the SBOM attached as an attestation.
- **Release notes** are the changelog section, unchanged.

## Signing

- Commits and tags are signed with an SSH key used only for signing (`git config gpg.format ssh`, `commit.gpgsign true`, `tag.gpgsign true`). GitHub shows them as verified once the public key is added as a signing key. Setting it up is the owner's step: the key must live on the owner's machine.
- Release artifacts are not signed with a long-lived key. Sigstore ties each signature to the workflow run that built it, so there is no key to lose or rotate.

## Tools

- The release workflow is generated with `dist` (cargo-dist) if its output passes `zizmor`, pins every action to a SHA and needs no write token outside the release job. Otherwise it is written by hand. The milestone decides with a dry run on a test tag.
- `release-plz` and `git-cliff` are not used. They write the changelog from commit subjects, and the changelog here is written for operators.
- Reproducible builds are the goal after 0.1.0: two builds of one tag give the same bytes, checked in CI.

## Supported versions

Before 1.0 only the latest release gets fixes, security fixes included. An operator upgrades to get them. The policy for 1.0 and later is decided before 1.0.
