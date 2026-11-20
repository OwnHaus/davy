#!/bin/sh
# Stop hook: an agent that changed Rust code cannot finish while scripts/check
# fails. Exit 2 sends the failure back to the agent instead of ending the turn.
cd "${CLAUDE_PROJECT_DIR:-$(git rev-parse --show-toplevel)}" || exit 0

# Second stop in a row: the agent already saw one failure, let it hand back.
if grep -q '"stop_hook_active": *true'; then
    exit 0
fi

changed=$( (git diff --name-only HEAD; git ls-files --others --exclude-standard) |
    grep -E '\.rs$|(^|/)Cargo\.(toml|lock)$|^(clippy|deny|rust-toolchain)\.toml$|^scripts/check$' || true)
[ -z "$changed" ] && exit 0

if ! out=$(scripts/check 2>&1); then
    printf '%s\n' "$out" | tail -60 >&2
    printf '\nscripts/check failed. Fix the code, not the check, then stop again.\n' >&2
    exit 2
fi
