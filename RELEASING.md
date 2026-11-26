# Releasing

No version has been released yet. `0.1.0` in `Cargo.toml` is unreleased. The milestone "Release 0.1.0" in `docs/design.md` builds what this file marks as planned. Until then this file is the spec for it.

## Versions

- Davy follows [Semantic Versioning](https://semver.org/). The public surface is the ingest routes, the MCP tools, the CLI commands and their `--json` output, the webhook body and the data directory.
- Before 1.0 a minor release may break the public surface and a patch release never does. 1.0 comes when the data directory and the webhook body are promised stable.
- A newer Davy opens every data directory an older one wrote and migrates it, only while no other process has it open. An older Davy refuses a newer one, and names the newer release and the backup taken before the upgrade. A release that migrates data says so in the changelog. Downgrades are not supported, so back up the data directory before an upgrade: going back means restoring that backup.
- A release is a tag `vX.Y.Z` on `main`, annotated and signed.

## Cadence

- A release ships when `## Unreleased` holds something an operator wants. There is no calendar.
- A fix for a confirmed vulnerability ships as a patch release within the times in `SECURITY.md`, ahead of anything else.

## Steps

1. `main` is clean, pushed and green in CI.
2. `scripts/release X.Y.Z` runs the gate and e2e, sets the version in `Cargo.toml` and `Cargo.lock`, turns `## Unreleased` into `## X.Y.Z (YYYY-MM-DD)` with an empty `## Unreleased` above it, moves the shipped items of `roadmap.json` from `"unreleased"` to `X.Y.Z` and rewrites the website with `node site/tools/roadmap.cjs`, commits `chore(release): release X.Y.Z` and creates the signed tag `vX.Y.Z`. The tag message is the changelog section. It refuses a tree with changes, a branch other than `main`, a `main` that differs from `origin/main`, an existing tag and a `## Unreleased` without an entry. It lists the `feat`, `fix` and `perf` commits since the last tag, to check each has its changelog line. `--unsigned` makes an annotated tag without a signature, for a test tag before signing is set up. `scripts/test-release` tests it in a throwaway repository.
3. `git push origin main vX.Y.Z`. The tag starts the release workflow.
4. The release workflow (`.github/workflows/release.yml`) builds each binary with `scripts/build-release` on a runner of its platform. Its publish job, the only one that can write, attests the files, pushes and signs the image and opens a draft release with the files below. Its verify job runs `scripts/verify-release` on the same files and the image. It gets the files as a workflow artifact, because a job that cannot write cannot see a draft.
5. The owner runs `scripts/verify-release vX.Y.Z` on their own machine, which downloads the draft itself, runs the e2e suite against a downloaded binary and publishes the draft.

A tag `vX.Y.Z-suffix`, such as `v0.1.0-test.1`, runs the same workflow and opens a draft prerelease with the `## Unreleased` notes. It tests the workflow, and its draft, tag and image are deleted after.

A release needs a public repository. GitHub keeps artifact attestations only for public repositories (or GitHub Enterprise Cloud), and Sigstore records every signature in a public log, which would name a private repository. So in a private repository a test tag runs unsigned: no attestations, no image signature, and the verify job runs `scripts/verify-release --unsigned`, which still checks the checksums, the dependency list and the image. The workflow refuses a release tag there.

## What a release contains

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

- The release workflow is written by hand. `dist` (cargo-dist) was the first choice, but it grants `contents: write` to the whole workflow (its `src/backend/ci/github.rs` sets it as a root permission), which breaks the rule that only the publish job can write, and it builds no container image.
- Linux binaries build in `rust:alpine` pinned by digest, natively on an x86_64 and an arm64 runner, because musl there is the system C library and the C dependencies (SQLite, zstd, aws-lc) build without a cross toolchain. The image copies those binaries and compiles nothing.
- The release workflow uses no cache, so nothing an earlier run left can end up in a release.
- `release-plz` and `git-cliff` do not write the changelog. They would copy commit subjects, and the changelog here is written for operators. `scripts/release` prints the `feat`, `fix` and `perf` commits since the last tag as a checklist instead.
- Reproducible builds are the goal after 0.1.0: two builds of one tag give the same bytes, checked in CI.

## Supported versions

Before 1.0 only the latest release gets fixes, security fixes included. An operator upgrades to get them. The policy for 1.0 and later is decided before 1.0.
