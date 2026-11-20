# Davy agent guide

Davy is one Rust binary that replaces Sentry and Better Stack. Read `docs/architecture.md` for where code lives and which invariants hold. Read `docs/design.md` for what each milestone does and why.

## Commands

- `scripts/check` is the gate. It runs fmt, clippy with every lint as an error, the tests, `cargo deny`, and the slop checks. The pre-commit hook, the Claude Stop hook and CI all run it. A change is done when it passes.
- `cargo test` runs unit and contract tests. `bash e2e/run.sh` runs real SDKs against the release binary. Run e2e when you touch `http.rs`, a parser or a wire format.
- After a fresh clone: `git config core.hooksPath .githooks`.

Mechanical rules live in `Cargo.toml` `[lints]`, `clippy.toml`, `deny.toml` and `scripts/check`. This file holds only what a tool cannot check.

## Never loosen a gate

- Never relax a lint, a threshold in `clippy.toml`, `deny.toml` or `scripts/check` to get a change through. Fix the code.
- When a lint is wrong for one spot, use `#[expect(lint, reason = "...")]` on the smallest item. The reason says why this spot is the exception. `#[allow]` does not compile.
- Never weaken, skip or delete a failing test to make it pass. If the test is wrong, say why in the commit message.

## Spec first

- New behavior starts as a section in `docs/design.md`: the data shape, the wire format, what "done" means. Then code. When the code teaches you the spec was wrong, fix the spec in the same commit.
- Name the data shape before writing logic. Types carry the invariants. A value that passed the boundary is trusted, so code behind `http.rs` and the parsers does not re-check it.

## Writing code

- Make the smallest change that solves the task. No speculative options, traits with one implementation, or layers for a future that is not in `docs/design.md`.
- Search before you write a helper. `git grep` the name and the shape. Time goes through `src/time.rs`, timestamps of logs through `logs::timestamp`.
- A new dependency needs a reason in the commit message, a license `deny.toml` allows, and no overlap with one already in `Cargo.toml`.
- Comments explain why, never what. No comment that restates the next line, narrates a step, or records history. A doc comment says what a caller must know that the signature cannot say.
- Errors carry context the reader needs (`format!("writer stopped: {e}")`). Never drop an error silently. A dropped one gets `#[expect(..., reason)]` saying why that is safe.
- Keep parsers pure: bytes in, value or error out. No clock reads, no I/O.
- Delete dead code instead of commenting it out or keeping it "for later".

## Tests

- Test behavior through the public function a caller uses. Assert literal expected values, not values computed by the code under test.
- Parsers are tested on fixtures captured from real clients (`e2e/fixtures/`). Never hand-write a fixture that claims to be from a client.
- A bug fix starts with a test that fails without the fix.
- Table-driven when cases share a shape. One behavior per test, named after the behavior.

## Numbers and claims

- A performance number in docs or a commit message says how it was measured: the command, the machine, the data. An estimate is labeled as an estimate.
- Never claim something works without running it. "Compiles" is not "works".

## Prose

- Docs, comments and commit messages use short declarative sentences. No long dash character, `scripts/check` rejects it. No filler ("robust", "seamless", "leverage").
- Commit messages say what changed and why, in the imperative.

## Git

- One logical change per commit. Formatting, refactors and behavior changes go in separate commits.
- Never commit generated data, `data/`, `target/` or secrets.
- Never force-push `main`.

## Open core

- This crate is AGPL and must stay usable as a library by the closed `davy-cloud` crate. Nothing here depends on cloud code. A cloud feature needs an extension point here, not a fork.
- Dependencies must be permissive or weak copyleft, so the cloud crate can link them. `deny.toml` enforces the list.

## When an agent gets something wrong

Add a rule only after a mistake actually happened. Prefer a lint, a test or a line in `scripts/check` over a sentence here. A sentence here is for judgment no tool can make. Keep this file under 100 lines.
