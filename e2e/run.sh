#!/bin/bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
cargo build --release -j 4
BIN="$ROOT/target/release/davy"
SCRATCH="$ROOT/.scratch/e2e-$$"
mkdir -p "$SCRATCH"
PID=''
cleanup() {
  if [ -n "$PID" ]; then kill "$PID" 2>/dev/null || true; wait "$PID" 2>/dev/null || true; fi
  rm -rf "$SCRATCH"
}
trap cleanup EXIT
PORT=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()')
URL="http://127.0.0.1:$PORT"
"$BIN" serve --data "$SCRATCH/data" --listen "127.0.0.1:$PORT" --public-url "$URL" >"$SCRATCH/server.log" 2>&1 &
PID=$!
for i in {1..100}; do
  if curl -s -o /dev/null "$URL/"; then break; fi
  sleep 0.1
done
for SDK in sentry-node-7 sentry-node-10; do
  DSN=$("$BIN" project create "$SDK" --data "$SCRATCH/data" --public-url "$URL")
  node "$ROOT/e2e/send.mjs" "$SDK" "$DSN"
  "$BIN" issues --data "$SCRATCH/data" --project "$SDK" --json > "$SCRATCH/issues.json"
  python3 - "$SCRATCH/issues.json" "$SDK" <<'PY'
import json,sys
issues=json.load(open(sys.argv[1]))
assert len(issues)==2,issues
assert sorted(i['count'] for i in issues)==[1,2],issues
assert {i['title']:i['count'] for i in issues}=={'TypeError: SDK test failure':2,'RangeError: SDK test failure':1},issues
print(sys.argv[2]+': verified 2 issues, counts 2 and 1')
PY
done
DSN=$("$BIN" project create legacy --data "$SCRATCH/data" --public-url "$URL")
KEY="${DSN#*://}"; KEY="${KEY%%@*}"
PROJECT="${DSN##*/}"
python3 - "$SCRATCH" <<'PY'
import gzip,pathlib,sys
p=pathlib.Path(sys.argv[1]); (p/'envelope.gz').write_bytes(gzip.compress(b'{}\n{"type":"event"}\n{"event_id":"11111111111111111111111111111111","message":"gzip event"}'))
PY
curl -fsS -H 'Content-Encoding: gzip' --data-binary @"$SCRATCH/envelope.gz" "$URL/api/$PROJECT/envelope/?sentry_key=$KEY" > "$SCRATCH/gzip-response.json"
curl -fsS -H 'Content-Type: application/json' -H "X-Sentry-Auth: Sentry sentry_key=$KEY, sentry_version=7" --data-binary '{"event_id":"22222222222222222222222222222222","message":"store event"}' "$URL/api/$PROJECT/store/" > "$SCRATCH/store-response.json"
"$BIN" issues --data "$SCRATCH/data" --project legacy --json > "$SCRATCH/issues.json"
python3 - "$SCRATCH" <<'PY'
import json,pathlib,sys
p=pathlib.Path(sys.argv[1]); issues=json.loads((p/'issues.json').read_text())
assert {i['title']:i['count'] for i in issues}=={'gzip event':1,'store event':1},issues
assert json.loads((p/'gzip-response.json').read_text())=={'id':'11111111111111111111111111111111'}
assert json.loads((p/'store-response.json').read_text())=={'id':'22222222222222222222222222222222'}
print('gzip envelope and /store/: verified 2 issues, count 1 each')
PY
"$BIN" event 1 --data "$SCRATCH/data" > "$SCRATCH/payload.json"
python3 - "$SCRATCH/payload.json" <<'PY'
import json,sys
p=json.load(open(sys.argv[1])); assert p['exception']['values'][-1]['type'] in ('TypeError','RangeError')
print('event CLI: verified stored JSON payload')
PY
echo 'PASS: all end-to-end assertions passed'
