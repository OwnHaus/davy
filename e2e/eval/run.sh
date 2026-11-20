#!/bin/bash
# The agent eval of milestone 7: a coding agent gets a fresh Debian
# container with systemd, a small Node app and the prompt in prompt.txt,
# and must finish with the app's test error visible over MCP. Then the
# app is started with `npm start` in a clean environment and must report
# its own error too.
# Usage: e2e/eval/run.sh codex|claude RUNS. Results go to .scratch/eval/.
# Needs Docker. Codex logs in with the ChatGPT login in ~/.codex/auth.json,
# copied into each container and removed with it. Claude Code logs in with
# CLAUDE_CODE_OAUTH_TOKEN from `claude setup-token`, passed by name so it
# appears in no command line.
set -euo pipefail
AGENT="${1:?agent: codex or claude}"
RUNS="${2:-1}"
case "$AGENT" in
  codex) ;;
  claude) [ -n "${CLAUDE_CODE_OAUTH_TOKEN:-}" ] || { echo "set CLAUDE_CODE_OAUTH_TOKEN, from claude setup-token" >&2; exit 2; } ;;
  *) echo "agent: codex or claude" >&2; exit 2 ;;
esac
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
STAGE="$ROOT/.scratch/eval/davy"
RESULTS="$ROOT/.scratch/eval/results"
# Emptied, not removed: Docker Desktop can mount a directory that was just
# removed and made again as the old one, and the build then cannot write.
mkdir -p "$STAGE/release" "$RESULTS"
rm -rf "$STAGE/docs" "$STAGE/README.md" "$STAGE/llms.txt" "$STAGE/release/"*
cp -R "$ROOT/docs" "$ROOT/README.md" "$ROOT/llms.txt" "$STAGE/"
docker run --rm -v "$ROOT":/src:ro -v davy-cargo:/usr/local/cargo/registry -v davy-rustup:/usr/local/rustup \
  -v davy-target:/target -v "$STAGE/release":/out -e CARGO_TARGET_DIR=/target -w /src rust:bookworm \
  bash -c 'cargo build --release --locked -j 4 && cp /target/release/davy /out/davy-linux && cd /out && sha256sum davy-linux > SHA256SUMS'
CODEX_VERSION="$(codex --version | awk '{print $2}')"
CLAUDE_VERSION="$(claude --version | awk '{print $1}')"
docker build -q --build-arg CODEX_VERSION="$CODEX_VERSION" --build-arg CLAUDE_VERSION="$CLAUDE_VERSION" \
  -t davy-eval "$ROOT/e2e/eval" > /dev/null
PROMPT="$(cat "$ROOT/e2e/eval/prompt.txt")"

for i in $(seq 1 "$RUNS"); do
  ID="$AGENT-$(date -u +%Y%m%dT%H%M%S)"
  OUT="$RESULTS/$ID"
  NAME="davy-eval-$$-$i"
  mkdir -p "$OUT"
  trap 'docker rm -f "$NAME" > /dev/null 2>&1 || true' EXIT
  docker run -d --name "$NAME" --privileged --cgroupns=host -v /sys/fs/cgroup:/sys/fs/cgroup:rw \
    --tmpfs /run --tmpfs /run/lock -v "$STAGE":/davy:ro -v "$ROOT/e2e/eval":/eval:ro davy-eval > /dev/null
  docker exec "$NAME" systemctl is-system-running --wait > /dev/null || true
  docker exec "$NAME" bash -euc '
    useradd --create-home --shell /bin/bash agent
    echo "agent ALL=(ALL) NOPASSWD:ALL" > /etc/sudoers.d/agent && chmod 440 /etc/sudoers.d/agent
    cp -R /eval/app /app && chown -R agent:agent /app
    runuser -u agent -- bash -c "cd /app && git init -q && git add -A && git -c user.name=dev -c user.email=dev@example.com commit -qm init"
    install -d -o agent -g agent -m 700 /home/agent/.codex'
  if [ "$AGENT" = codex ]; then
    docker cp "$HOME/.codex/auth.json" "$NAME":/home/agent/.codex/auth.json > /dev/null
    docker exec "$NAME" bash -c 'chown agent:agent /home/agent/.codex/auth.json && chmod 600 /home/agent/.codex/auth.json'
  fi
  START=$(date +%s)
  set +e
  if [ "$AGENT" = codex ]; then
    docker exec -u agent -w /app -e HOME=/home/agent "$NAME" timeout 2400 \
      codex exec --json --dangerously-bypass-approvals-and-sandbox --skip-git-repo-check -C /app \
      -o /home/agent/last.txt "$PROMPT" > "$OUT/events.jsonl" 2> "$OUT/stderr.txt"
  else
    docker exec -u agent -w /app -e HOME=/home/agent -e CLAUDE_CODE_OAUTH_TOKEN "$NAME" timeout 2400 \
      claude -p --output-format stream-json --verbose --dangerously-skip-permissions "$PROMPT" \
      > "$OUT/events.jsonl" 2> "$OUT/stderr.txt"
  fi
  EXIT=$?
  set -e
  echo $(( $(date +%s) - START )) > "$OUT/seconds"
  echo "$EXIT" > "$OUT/exit"
  docker exec "$NAME" cat /home/agent/last.txt > "$OUT/last.txt" 2> /dev/null || true
  docker exec "$NAME" runuser -u davy -- davy doctor --data /var/lib/davy --json > "$OUT/doctor.json" 2> /dev/null || true
  docker exec "$NAME" runuser -u davy -- davy feedback --data /var/lib/davy --json > "$OUT/feedback.json" 2> /dev/null || true
  # The app must report its own error when started the way it starts,
  # with no SENTRY_DSN left in a shell: GET /orders/42 throws.
  docker exec -u agent -w /app "$NAME" bash -c 'env -i HOME=/home/agent PATH=/usr/local/bin:/usr/bin:/bin PORT=3999 \
    setsid npm start > /tmp/app.log 2>&1 < /dev/null & sleep 3; curl -s -m 5 http://127.0.0.1:3999/orders/42; sleep 5' \
    > /dev/null 2>&1 || true
  docker exec "$NAME" cat /tmp/app.log > "$OUT/app.log" 2> /dev/null || true
  docker exec "$NAME" node /eval/check.mjs > "$OUT/mcp.json" 2> "$OUT/mcp-error.txt" || true
  docker exec "$NAME" bash -c 'grep -A4 "mcp_servers" /home/agent/.codex/config.toml; cat /app/.mcp.json; grep -o "\"mcpServers\":{[^}]*}" /home/agent/.claude.json' > "$OUT/agent-mcp.txt" 2> /dev/null || true
  docker exec "$NAME" bash -c 'grep -c DAVY_TOKEN /home/agent/.bashrc /home/agent/.profile 2>/dev/null; grep -l "@sentry/node" /app/package.json' > "$OUT/app.txt" 2> /dev/null || true
  docker exec -u agent -w /app "$NAME" bash -c 'git status --short && git diff && cat .env' > "$OUT/app.diff" 2> /dev/null || true
  docker rm -f "$NAME" > /dev/null
  python3 "$ROOT/e2e/eval/verdict.py" "$OUT"
done
