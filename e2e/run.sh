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
TOKEN=$(python3 -c 'import secrets; print(secrets.token_urlsafe(32))')
DAVY_TOKEN="$TOKEN" "$BIN" serve --data "$SCRATCH/data" --listen "127.0.0.1:$PORT" --public-url "$URL" >"$SCRATCH/server.log" 2>&1 &
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
DSN=$("$BIN" project create logs --data "$SCRATCH/data" --public-url "$URL")
KEY="${DSN#*://}"; KEY="${KEY%%@*}"
node "$ROOT/e2e/send-logs.mjs" "$DSN"
for i in {1..50}; do
  "$BIN" logs --data "$SCRATCH/data" --project logs --since 10m --json > "$SCRATCH/logs.ndjson"
  if [ "$(wc -l < "$SCRATCH/logs.ndjson")" -ge 10 ]; then break; fi
  sleep 0.2
done
"$BIN" logs --data "$SCRATCH/data" --project logs --since 10m --level error --json 'timeout OR exception OR attributes.provider:acme' > "$SCRATCH/errors.ndjson"
python3 - "$SCRATCH" <<'PY'
import json,pathlib,sys
p=pathlib.Path(sys.argv[1])
rows=[json.loads(l) for l in (p/'logs.ndjson').read_text().splitlines()]
got=sorted((r['source'],r['level'],r['message']) for r in rows)
want=sorted([
  ('logtail','info','logtail request completed'),
  ('logtail','error','logtail unhandled exception'),
  ('logtail','debug','logtail debug line'),
  ('sentry','info','sentry order placed'),
  ('sentry','warn','sentry cart c-77 is over the limit'),
  ('sentry','error','sentry payment provider timed out'),
  ('otlp','info','otlp proto GET /cart 200'),
  ('otlp','error','{"error":"otlp proto db timeout","ms":5000}'),
  ('otlp','info','otlp json GET /cart 200'),
  ('otlp','error','{"error":"otlp json db timeout","ms":5000}'),
])
assert got==want,got
ts=[r['timestamp_ns'] for r in rows]
assert ts==sorted(ts),'oldest first'
by={r['message']:r for r in rows}
assert by['logtail request completed']['attributes']['res']=={'statusCode':200}
assert by['sentry order placed']['attributes']['order.id']=='ord-1001'
proto=by['otlp proto GET /cart 200']
assert (proto['service'],proto['trace_id'],proto['span_id'])==('checkout','5b8efff798038103d269b633813fc60c','eee19b7ec3c1b174'),proto
errors=sorted(json.loads(l)['message'] for l in (p/'errors.ndjson').read_text().splitlines())
assert errors==sorted(['logtail unhandled exception','sentry payment provider timed out','{"error":"otlp proto db timeout","ms":5000}','{"error":"otlp json db timeout","ms":5000}']),errors
print('logs: verified 10 rows from 4 senders, oldest first, level and text filters')
PY
STATUS=$(curl -s -o /dev/null -w '%{http_code}' -H 'Authorization: Bearer wrong' -H 'Content-Type: application/json' --data '{"message":"x"}' "$URL/")
[ "$STATUS" = 403 ] || { echo "wrong bearer gave $STATUS"; exit 1; }
STATUS=$(curl -s -o /dev/null -w '%{http_code}' -H "Authorization: Bearer $KEY" -H 'Content-Type: application/json' --data '{"message":"curl line","level":"warn"}' "$URL/")
[ "$STATUS" = 202 ] || { echo "logtail JSON gave $STATUS"; exit 1; }
echo 'logs: wrong bearer 403, Logtail JSON 202'
STATUS=$(curl -s -o /dev/null -w '%{http_code}' -H 'Content-Type: application/json' --data '{}' "$URL/mcp")
[ "$STATUS" = 401 ] || { echo "MCP without token gave $STATUS"; exit 1; }
node "$ROOT/e2e/mcp.mjs" "$URL" "$TOKEN"
"$BIN" feedback --data "$SCRATCH/data" | grep -qx '  tried: e2e tried' || { echo 'davy feedback does not list the MCP report'; exit 1; }
ISSUE=$("$BIN" sql --data "$SCRATCH/data" "SELECT i.id FROM issues i JOIN projects p ON p.id = i.project_id WHERE p.slug = 'sentry-node-7' AND i.title LIKE 'RangeError%'" | tail -n 1)
"$BIN" context "$ISSUE" --data "$SCRATCH/data" > "$SCRATCH/context.json"
"$BIN" resolve "$ISSUE" --data "$SCRATCH/data" --release e2e@2
"$BIN" sql --data "$SCRATCH/data" --json "SELECT status, resolved_in_release FROM issues WHERE id = $ISSUE" > "$SCRATCH/resolved.json"
python3 - "$SCRATCH" <<'PY'
import json,pathlib,sys
p=pathlib.Path(sys.argv[1])
ctx=json.loads((p/'context.json').read_text())
assert ctx['issue']['title']=='RangeError: SDK test failure',ctx['issue']
assert ctx['latest_event']['exceptions'][-1]['type']=='RangeError'
assert json.loads((p/'resolved.json').read_text())['rows']==[['resolved','e2e@2']]
print('agent CLI: verified sql, context and resolve')
PY
DSN=$("$BIN" project create shop --data "$SCRATCH/data" --public-url "$URL")
node "$ROOT/e2e/send-sourcemapped.mjs" "$SCRATCH/shop" "$URL" "$TOKEN" "$DSN" 2>"$SCRATCH/sentry-cli.log" || { cat "$SCRATCH/sentry-cli.log"; exit 1; }
ISSUE=$("$BIN" sql --data "$SCRATCH/data" "SELECT i.id FROM issues i JOIN projects p ON p.id = i.project_id WHERE p.slug = 'shop'" | tail -n 1)
"$BIN" context "$ISSUE" --data "$SCRATCH/data" > "$SCRATCH/context.json"
python3 - "$SCRATCH/context.json" <<'PY'
import json,sys
ctx=json.load(open(sys.argv[1]))
frames=ctx['latest_event']['exceptions'][-1]['frames']
top=[f for f in frames if f['in_app']][-1]
assert (top['function'],top['filename'],top['lineno'],top['context_line'])==('parseOrder','../src/app.js',5,'  return order.items.length'),top
assert ctx['issue']['title'].startswith('TypeError: Cannot read properties of undefined'),ctx['issue']
print('source maps: sentry-cli upload, symbolicated top frame parseOrder at ../src/app.js:5')
PY
node "$ROOT/e2e/alerts.mjs" "$BIN" "$SCRATCH/alerts"
echo 'PASS: all end-to-end assertions passed'

