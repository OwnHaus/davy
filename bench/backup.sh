#!/bin/bash
# Usage: bench/backup.sh [log rows] [hours] [events]
# Fills a data directory with synthetic logs over the last HOURS, written
# straight into the store, and error events sent to a server, then times
# davy backup while the server takes logs, the pause it reports, verify
# and restore, and zstd level 1 against level 3 on the same tar. Needs
# free disk of about four times the store. See "Backup and restore" in
# docs/design.md.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
ROWS="${1:-20000000}"; HOURS="${2:-96}"; EVENTS="${3:-100000}"
cargo build --release -j 4 --bin davy --example fill_logs --example load_logs --example load_events 2>&1 | tail -1
BIN="$ROOT/target/release/davy"
EX="$ROOT/target/release/examples"
SCRATCH="${BENCH_DIR:-$ROOT/.scratch}/backup-bench-$$"
mkdir -p "$SCRATCH"
PID=''
stop() {
  if [ -n "$PID" ]; then kill "$PID" 2>/dev/null || true; wait "$PID" 2>/dev/null || true; PID=''; fi
}
cleanup() { stop; rm -rf "$SCRATCH"; }
trap cleanup EXIT
now() { python3 -c 'import time; print(time.time())'; }
since() { python3 -c "print(f'{$(now)-$1:.1f}')"; }
PORT=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()')
URL="http://127.0.0.1:$PORT"
DATA="$SCRATCH/data"
DSN=$("$BIN" project create bench --data "$DATA" --public-url "$URL")
KEY="${DSN#*://}"; KEY="${KEY%%@*}"; PROJECT="${DSN##*/}"
"$BIN" project quota bench --data "$DATA" --spike off --spike-logs off > /dev/null
"$EX/fill_logs" "$DATA" "$ROWS" "$HOURS" 1 | tail -1
serve() {
  DAVY_TOKEN=bench-token-bench-token-bench-token \
    "$BIN" serve --data "$DATA" --listen "127.0.0.1:$PORT" --public-url "$URL" --hot-days 2 >>"$SCRATCH/server.log" 2>&1 &
  PID=$!
  for _ in {1..300}; do curl -s -o /dev/null "$URL/" && return; sleep 0.1; done
  echo "the server did not start"; cat "$SCRATCH/server.log"; exit 1
}
serve
"$EX/load_events" "127.0.0.1:$PORT" "$PROJECT" "$KEY" "$EVENTS" 8 | tail -1
# Let the cold tier turn the days past --hot-days cold before measuring.
for _ in {1..900}; do
  [ "$(ls "$DATA/logs" | grep -c '^20')" -le 3 ] && break
  sleep 1
done
echo "store: $(du -sh "$DATA" | cut -f1), database $(du -sh "$DATA/davy.sqlite" | cut -f1), logs $(du -sh "$DATA/logs" | cut -f1), $(find "$DATA" -type f | wc -l | tr -d ' ') files"
echo "load before: $(sysctl -n vm.loadavg)"

echo "== ingest alone"
"$EX/load_logs" "127.0.0.1:$PORT" "$KEY" 3000000 1000 4 2>/dev/null | grep accepted

echo "== backup while the server takes logs"
"$EX/load_logs" "127.0.0.1:$PORT" "$KEY" 3000000 1000 4 > "$SCRATCH/load.txt" 2> "$SCRATCH/load.err" &
LOAD=$!
# The load generates its batches first; back up once it sends them.
until grep -q generated "$SCRATCH/load.err"; do sleep 0.2; done
sleep 2
START=$(now)
"$BIN" backup --data "$DATA" "$SCRATCH/one.tar.zst"
echo "backup took $(since "$START") s"
wait "$LOAD"
grep accepted "$SCRATCH/load.txt"

echo "== backup with no ingest, five times, for the pause"
for i in 1 2 3 4 5; do
  "$BIN" backup --data "$DATA" "$SCRATCH/quiet.tar.zst" | sed -n 's/.*\(in [0-9.]* s; the log writer paused for [0-9]* ms\).*/\1/p'
  rm -f "$SCRATCH/quiet.tar.zst"
done

echo "== verify"
START=$(now)
"$BIN" backup verify "$SCRATCH/one.tar.zst"
echo "verify took $(since "$START") s"
stop

echo "== restore"
START=$(now)
"$BIN" restore --data "$SCRATCH/restored" "$SCRATCH/one.tar.zst"
echo "restore took $(since "$START") s"
rm -rf "$SCRATCH/restored"

echo "== zstd level 1 against 3 on the same tar"
zstd -dc "$SCRATCH/one.tar.zst" > "$SCRATCH/one.tar"
echo "tar: $(du -h "$SCRATCH/one.tar" | cut -f1)"
for level in 1 3; do
  START=$(now)
  zstd -q -T1 "-$level" -c "$SCRATCH/one.tar" > "$SCRATCH/level.zst"
  echo "level $level: $(du -h "$SCRATCH/level.zst" | cut -f1) in $(since "$START") s"
done
echo "load after: $(sysctl -n vm.loadavg)"
