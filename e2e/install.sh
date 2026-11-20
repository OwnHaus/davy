#!/bin/bash
# Follows docs/install.md, connect/node.md and connect/agents.md in Debian
# with systemd, the way a coding agent would, then checks doctor, the issue
# from the test error and MCP. Needs Docker.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
NAME="davy-install-$$"
cleanup() { docker rm -f "$NAME" > /dev/null 2>&1 || true; }
trap cleanup EXIT
docker run --rm -v "$ROOT":/src:ro -v davy-cargo:/usr/local/cargo/registry -v davy-rustup:/usr/local/rustup \
  -v davy-target:/target -e CARGO_TARGET_DIR=/target -w /src rust:bookworm \
  bash -c 'cargo build --release --locked -j 4 && cp /target/release/davy /target/davy-linux'
docker build -q -t davy-install "$ROOT/e2e/install" > /dev/null
docker run -d --name "$NAME" --privileged --cgroupns=host -v /sys/fs/cgroup:/sys/fs/cgroup:rw \
  --tmpfs /run --tmpfs /run/lock -v davy-target:/target:ro \
  -v "$ROOT/docs":/docs:ro -v "$ROOT/e2e/install":/e2e:ro davy-install > /dev/null
docker exec "$NAME" systemctl is-system-running --wait > /dev/null || true
docker exec "$NAME" bash /e2e/inside.sh
