# Contributing

Davy is built by its owner and the coding agents that work for the owner. Outside contributions open with the public release and need the CLA described in `docs/design.md`. `AGENTS.md` holds the rules for writing code. This file holds the flow around it: commits, the changelog, dependencies and review. Releases are in `RELEASING.md`, the security program in `SECURITY.md`.

## Setup after a clone

```sh
git config core.hooksPath .githooks
brew install cargo-deny zizmor osv-scanner
```

The web UI needs Node 24 and pnpm 11. `scripts/ui` builds it into `ui/dist/`, which the next `cargo build` embeds. Without Node, `scripts/check` skips the UI and CI still runs it. While you work on the UI, `cd ui && pnpm exec vite` serves it with hot reload and sends the API to the Davy at `DAVY_URL` (default `http://127.0.0.1:8000`).

The pre-commit hook runs `scripts/check`. The commit-msg hook runs `scripts/commit-msg`.

## The flow

- `main` is always releasable. Every commit on it passes `scripts/check`, and CI runs the gate, e2e and the commit check on every push.
- Today the owner commits to `main` directly, because the hooks run the same gate as CI. With the public release `main` gets a GitHub ruleset: changes land through pull requests, CI must be green, commits must be signed, history stays linear, and force pushes and deletion are blocked.
- A branch is named after what it does, short and in kebab case (`smtp-alerts`).
- Never force-push `main`.

## Commits

- One logical change per commit. A formatting change, a refactor and a behavior change are three commits. Each commit builds and passes the gate on its own, so `git bisect` works.
- The subject follows [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/) with a required scope: `type(scope): description`, at most 72 characters, as in `feat(alert): send alerts by e-mail over SMTP`. The description is imperative, starts with a lowercase verb, has no period, and names the change a reader cares about, not the files it touches.
- A `!` after the scope marks a change that breaks the public surface (`RELEASING.md`, "Versions"): `fix(http)!: refuse bodies over 1 MiB`. The body then says what an operator must change.
- The body follows a blank line. It says why, what else was considered and what it costs. A new dependency names its license and why nothing in `Cargo.toml` covers it. A number says how it was measured.
- The repo owner is the only author. No `Co-Authored-By` trailer for a model, no "Generated with" line.
- Commits are signed once the owner's signing key is set up (see `RELEASING.md`, "Signing"). From the public release on, the ruleset refuses unsigned commits.

`scripts/commit-msg` checks every rule a tool can check. The changelog is still written by hand for operators. `git log --oneline v0.1.0.. | grep -E '^\w+ (feat|fix|perf)'` lists what to write about.

### Types

| Type | For |
|---|---|
| `feat` | Something an operator or an agent can do that it could not before |
| `fix` | Behavior that was wrong and now is right, with a test that failed before |
| `perf` | The same behavior, measurably faster or smaller. The body says how it was measured |
| `refactor` | A change of structure with no change of behavior |
| `test` | Tests only |
| `docs` | Documentation and specs only |
| `build` | Cargo, the toolchain, dependencies and how the binary is built |
| `ci` | GitHub workflows, hooks and `scripts/` |
| `chore` | Anything else that changes no behavior, such as a release commit |
| `revert` | Undoes an earlier commit, named in the body |

### Scopes

The scope says which part of the code changes, not who the change is for. A change to `net.rs` for the cloud is `feat(net)`. The closed `davy-cloud` repo has its own scopes. When a commit touches two scopes, it is usually two commits. When it cannot be split, use the scope of the behavior it changes.

| Scope | Covers |
|---|---|
| `http` | `http.rs` and `token.rs`: routes, credentials, caps, decompression, the server boundary |
| `errors` | Error events: `envelope.rs`, `event.rs`, `grouping.rs`, issues and their status |
| `logs` | `src/logs/`: adapters, the Tantivy store, retention and search |
| `sourcemaps` | `src/sourcemaps/` and `upload.rs` |
| `mcp` | `mcp.rs`, `query.rs` and `context.rs`: what agents see |
| `monitor` | `src/monitor/`: HTTP checks and heartbeats |
| `alert` | `src/alert/`: channels, notices and delivery |
| `core` | What modules share: the SQLite schema, migrations and writer in `store.rs`, `secret.rs`, `time.rs`, `tasks.rs`, `lib.rs` |
| `net` | `net.rs`: the outbound client and the addresses it may reach |
| `cli` | `main.rs` and `src/install/`: commands, flags, output, and setup of a machine |
| `ui` | The web UI |
| `e2e` | `e2e/`: real clients against the binary, and captured fixtures |
| `bench` | `bench/` and `examples/` |
| `deps` | Dependency updates, as `build(deps): update tokio to 1.52` |
| `release` | Versions, the changelog at release time, the release workflow and `scripts/release` |
| `security` | `SECURITY.md`, the threat model and security tooling |
| `repo` | Repo-wide rules and docs: `AGENTS.md`, this file, the gate, hooks and CI |

## Changelog

- A change an operator would notice adds a line under `## Unreleased` in `CHANGELOG.md`, in the same commit. Internal refactors and test changes do not.
- The sections are those of [Keep a Changelog](https://keepachangelog.com/en/1.1.0/): Added, Changed, Deprecated, Removed, Fixed, Security.
- A line says what the operator can do now or must do now, not how the code changed. A breaking change starts with **Breaking** and says what to change.

## Dependencies

- Dependabot opens grouped pull requests every week for Cargo, the e2e packages, the web UI packages and the GitHub Actions. They are never merged on GitHub. The owner applies the update locally, runs the gate and e2e, and commits it as `build(deps): update <crate> to <version>`.
- Actions are pinned to a full commit SHA with the version in a comment.
- A Rust toolchain bump is its own commit that changes `rust-toolchain.toml` and fixes what new lints find.

## Tests

- A test calls the code the way its users do, through a real server on loopback, a real SQLite file or a real index, and compares the result with a literal value. Mock crates are banned in `deny.toml`.
- A test waits for the event it expects, never for a fixed time. `scripts/test-sleeps` lists the few sleeps that remain, per file and with the reason. `scripts/check` fails when a file has more or fewer, so the list only shrinks.
- No test is skipped with `#[ignore]`. A test that cannot pass is fixed or deleted.
- A `fix` commit adds a test that fails without the fix.

## Review

- Every change passes `scripts/check`. A change to `http.rs`, a parser, a wire format, `ui/` or `src/ui/` also passes `bash e2e/run.sh`, and a parser change also passes `fuzz/docker 60` (see `fuzz/README.md`). A change to `docs/install.md`, `docs/connect/` or `src/install/` also passes `bash e2e/install.sh`, which follows the docs in a Debian container with systemd (needs Docker).
- A change to a trust boundary gets a security read before it is pushed, by a model that did not write it. The trust boundaries are `http.rs`, `upload.rs`, `mcp.rs`, `token.rs`, `query.rs`, `net.rs`, the parsers and anything that touches a credential. A finding becomes a fix or a reason in the commit body why it is not one.
- Each milestone ends with a security review of everything since the last one. See `SECURITY.md`.
