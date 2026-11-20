# Contributing

Davy is built by its owner and the coding agents that work for the owner. Outside contributions open with the public release and need the CLA described in `docs/design.md`. `AGENTS.md` holds the rules for writing code. This file holds the flow around it: commits, the changelog, dependencies and review. Releases are in `RELEASING.md`, the security program in `SECURITY.md`.

## Setup after a clone

```sh
git config core.hooksPath .githooks
brew install cargo-deny zizmor
```

The pre-commit hook runs `scripts/check`. The commit-msg hook runs `scripts/commit-msg`.

## The flow

- `main` is always releasable. Every commit on it passes `scripts/check`, and CI runs the gate, e2e and the commit check on every push.
- Today the owner commits to `main` directly, because the hooks run the same gate as CI. With the public release `main` gets a GitHub ruleset: changes land through pull requests, CI must be green, commits must be signed, history stays linear, and force pushes and deletion are blocked.
- A branch is named after what it does, short and in kebab case (`smtp-alerts`).
- Never force-push `main`.

## Commits

- One logical change per commit. A formatting change, a refactor and a behavior change are three commits. Each commit builds and passes the gate on its own, so `git bisect` works.
- The subject says what the commit does, in the imperative, starting with a capital letter, at most 72 characters, without a period: `Send alerts by e-mail over SMTP`. It names the change a reader cares about, not the files it touches.
- The body follows a blank line. It says why, what else was considered and what it costs. A new dependency names its license and why nothing in `Cargo.toml` covers it. A number says how it was measured.
- The repo owner is the only author. No `Co-Authored-By` trailer for a model, no "Generated with" line.
- Commits are signed once the owner's signing key is set up (see `RELEASING.md`, "Signing"). From the public release on, the ruleset refuses unsigned commits.

`scripts/commit-msg` checks every rule a tool can check. Davy does not use Conventional Commits. The changelog is written by hand for operators, so a `feat:` prefix would feed no tool, and the subject already says what changed.

## Changelog

- A change an operator would notice adds a line under `## Unreleased` in `CHANGELOG.md`, in the same commit. Internal refactors and test changes do not.
- The sections are those of [Keep a Changelog](https://keepachangelog.com/en/1.1.0/): Added, Changed, Deprecated, Removed, Fixed, Security.
- A line says what the operator can do now or must do now, not how the code changed. A breaking change starts with **Breaking** and says what to change.

## Dependencies

- Dependabot opens grouped pull requests every week for Cargo, the e2e packages and the GitHub Actions. They are never merged on GitHub. The owner applies the update locally, runs the gate and e2e, and commits it as `Update <crate> to <version>`.
- Actions are pinned to a full commit SHA with the version in a comment.
- A Rust toolchain bump is its own commit that changes `rust-toolchain.toml` and fixes what new lints find.

## Review

- Every change passes `scripts/check`. A change to `http.rs`, a parser or a wire format also passes `bash e2e/run.sh`.
- A change to a trust boundary gets a security read before it is pushed, by a model that did not write it. The trust boundaries are `http.rs`, `upload.rs`, `mcp.rs`, `token.rs`, `query.rs`, `net.rs`, the parsers and anything that touches a credential. A finding becomes a fix or a reason in the commit body why it is not one.
- Each milestone ends with a security review of everything since the last one. See `SECURITY.md`.
