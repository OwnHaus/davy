#!/bin/bash
# The agent eval of milestone 7: a coding agent gets a fresh Debian
# container with systemd, a small Node app and the prompt in prompt.txt,
# and must finish with the app's test error visible over MCP.
# Usage: e2e/eval/run.sh codex RUNS. Results go to .scratch/eval/.
# Needs Docker and a ChatGPT login in ~/.codex/auth.json, which is copied
# into each container and removed with it.
set -euo pipefail
AGENT="${1:?agent: codex}"
RUNS="${2:-1}"
[ "$AGENT" = codex ] || { echo "only codex is wired up" >&2; exit 2; }
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
STAGE="$ROOT/.scratch/eval/davy"
RESULTS="$ROOT/.scratch/eval/results"
rm -rf "$STAGE" && mkdir -p "$STAGE/release" "$RESULTS"
cp -R "$ROOT/docs" "$ROOT/README.md" "$ROOT/llms.txt" "$STAGE/"
docker run --rm -v "$ROOT":/src:ro -v davy-cargo:/usr/local/cargo/registry -v davy-rustup:/usr/local/rustup \
  -v davy-target:/target -v "$STAGE/release":/out -e CARGO_TARGET_DIR=/target -w /src rust:bookworm \
  bash -c 'cargo build --release --locked -j 4 && cp /target/release/davy /out/davy-linux && cd /out && sha256sum davy-linux > SHA256SUMS'
CODEX_VERSION="$(codex --version | awk '{print $2}')"
docker build -q --build-arg CODEX_VERSION="$CODEX_VERSION" -t davy-eval "$ROOT/e2e/eval" > /dev/null
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
  docker cp "$HOME/.codex/auth.json" "$NAME":/home/agent/.codex/auth.json > /dev/null
  docker exec "$NAME" bash -c 'chown agent:agent /home/agent/.codex/auth.json && chmod 600 /home/agent/.codex/auth.json'
  START=$(date +%s)
  set +e
  docker exec -u agent -w /app -e HOME=/home/agent "$NAME" timeout 2400 \
    codex exec --json --dangerously-bypass-approvals-and-sandbox --skip-git-repo-check -C /app \
    -o /home/agent/last.txt "$PROMPT" > "$OUT/events.jsonl" 2> "$OUT/stderr.txt"
  EXIT=$?
  set -e
  echo $(( $(date +%s) - START )) > "$OUT/seconds"
  echo "$EXIT" > "$OUT/exit"
  docker exec "$NAME" cat /home/agent/last.txt > "$OUT/last.txt" 2> /dev/null || true
  docker exec "$NAME" runuser -u davy -- davy doctor --data /var/lib/davy --json > "$OUT/doctor.json" 2> /dev/null || true
  docker exec "$NAME" runuser -u davy -- davy feedback --data /var/lib/davy --json > "$OUT/feedback.json" 2> /dev/null || true
  docker exec "$NAME" node /eval/check.mjs > "$OUT/mcp.json" 2> "$OUT/mcp-error.txt" || true
  docker exec "$NAME" bash -c 'grep -A4 "mcp_servers" /home/agent/.codex/config.toml' > "$OUT/agent-mcp.txt" 2> /dev/null || true
  docker exec "$NAME" bash -c 'grep -c DAVY_TOKEN /home/agent/.bashrc /home/agent/.profile 2>/dev/null; grep -l "@sentry/node" /app/package.json' > "$OUT/app.txt" 2> /dev/null || true
  docker exec "$NAME" bash -c 'cd /app && git status --short && git diff' > "$OUT/app.diff" 2> /dev/null || true
  docker rm -f "$NAME" > /dev/null
  python3 "$ROOT/e2e/eval/verdict.py" "$OUT"
done
