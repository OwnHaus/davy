#!/bin/bash
# Usage: bench/load-events.sh [events] [connections] [refused|spiked]
# Starts `davy serve` with spike protection off for its project and sends
# synthetic error events over the store route. A `200` means the event is
# committed, so the rate is what the store sustains. With `refused` the
# project has a quota of one event, and each event is sent once, so the
# rate is what a `429` past a quota costs; with `spiked` the project has a
# spike rate of one error event a second, for a `429` past the bucket.
# Run it at normal priority:
# under `taskpolicy -b` it measures the scheduler.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
EVENTS="${1:-50000}"; CONNS="${2:-8}"; MODE="${3:-}"
cargo build --release -j 4 --bin davy --example load_events 2>&1 | tail -1
BIN="$ROOT/target/release/davy"
SCRATCH="$ROOT/.scratch/load-events-$$"
mkdir -p "$SCRATCH"
PID=''
cleanup() {
  if [ -n "$PID" ]; then kill "$PID" 2>/dev/null || true; wait "$PID" 2>/dev/null || true; fi
  rm -rf "$SCRATCH"
}
trap cleanup EXIT
PORT=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()')
URL="http://127.0.0.1:$PORT"
DSN=$("$BIN" project create load --data "$SCRATCH/data" --public-url "$URL")
KEY="${DSN#*://}"; KEY="${KEY%%@*}"; PROJECT="${DSN##*/}"
"$BIN" project quota load --data "$SCRATCH/data" --spike off > /dev/null
ONCE=''
case "$MODE" in
  refused) "$BIN" project quota load --data "$SCRATCH/data" --errors 1 > /dev/null; ONCE=once ;;
  spiked) "$BIN" project quota load --data "$SCRATCH/data" --spike 1 > /dev/null; ONCE=once ;;
esac
DAVY_TOKEN=$(python3 -c 'import secrets; print(secrets.token_urlsafe(32))') \
  "$BIN" serve --data "$SCRATCH/data" --listen "127.0.0.1:$PORT" --public-url "$URL" >"$SCRATCH/server.log" 2>&1 &
PID=$!
for i in {1..100}; do curl -s -o /dev/null "$URL/" && break; sleep 0.1; done
echo "load before: $(sysctl -n vm.loadavg)"
"$ROOT/target/release/examples/load_events" "127.0.0.1:$PORT" "$PROJECT" "$KEY" "$EVENTS" "$CONNS" $ONCE
echo "load after: $(sysctl -n vm.loadavg)"
