#!/bin/bash
# Usage: bench/load-logs.sh [rows] [batch] [connections]
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
ROWS="${1:-1000000}"; BATCH="${2:-1000}"; CONNS="${3:-4}"
cargo build --release -j 4 --bin davy --example load_logs 2>&1 | tail -1
BIN="$ROOT/target/release/davy"
SCRATCH="$ROOT/.scratch/load-$$"
mkdir -p "$SCRATCH"
PID=''
cleanup() {
  if [ -n "$PID" ]; then pkill -TERM -P "$PID" 2>/dev/null || true; wait "$PID" 2>/dev/null || true; fi
  rm -rf "$SCRATCH"
}
trap cleanup EXIT
PORT=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()')
URL="http://127.0.0.1:$PORT"
DSN=$("$BIN" project create load --data "$SCRATCH/data" --public-url "$URL")
KEY="${DSN#*://}"; KEY="${KEY%%@*}"
# The rate measured is the server's, not the project's spike rate for logs,
# which is off by default; set here so a changed default does not skew it.
"$BIN" project quota load --data "$SCRATCH/data" --spike-logs off > /dev/null
/usr/bin/time -l "$BIN" serve --data "$SCRATCH/data" --listen "127.0.0.1:$PORT" --public-url "$URL" >"$SCRATCH/server.log" 2>&1 &
PID=$!
for i in {1..100}; do curl -s -o /dev/null "$URL/" && break; sleep 0.1; done
echo "load before: $(sysctl -n vm.loadavg)"
"$ROOT/target/release/examples/load_logs" "127.0.0.1:$PORT" "$KEY" "$ROWS" "$BATCH" "$CONNS" | tee "$SCRATCH/load.txt"
SENT=$(python3 -c 'import time; print(time.time())')
pkill -TERM -P "$PID"; wait "$PID" || true; PID=''
DRAINED=$(python3 -c 'import time; print(time.time())')
echo "load after: $(sysctl -n vm.loadavg)"
echo "server drained and committed $(python3 -c "print(round($DRAINED-$SENT,2))")s after the last 202"
grep -E 'real|maximum resident|log writer' "$SCRATCH/server.log" || true
RAW=$(sed -n 's/^raw_ndjson_bytes=//p' "$SCRATCH/load.txt")
DISK=$(( $(du -sk "$SCRATCH/data/logs" | cut -f1) * 1024 ))
python3 -c "print(f'disk {$DISK/1e6:.1f} MB, raw ndjson {$RAW/1e6:.1f} MB, disk/raw {$DISK/$RAW:.2f}x')"
ls "$SCRATCH/data/logs"
for Q in "" "timeout" "attributes.req.method:post AND attributes.res.statusCode:500"; do
  START=$(python3 -c 'import time; print(time.time())')
  if [ -n "$Q" ]; then ARGS=("$Q"); else ARGS=(); fi
  N=$("$BIN" logs --data "$SCRATCH/data" --since 2d --limit 100 --json ${ARGS[@]+"${ARGS[@]}"} | wc -l | tr -d ' ')
  END=$(python3 -c 'import time; print(time.time())')
  echo "query '${Q:-<none>}': $N rows in $(python3 -c "print(round(($END-$START)*1000))") ms (process start included)"
done
"$BIN" logs --data "$SCRATCH/data" --since 2d --limit 3 --level error
