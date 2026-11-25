#!/bin/bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
# DAVY_BIN runs the suite against another binary, such as a release download
# or e2e/davy-image, instead of building one.
if [ -n "${DAVY_BIN:-}" ]; then
  BIN="$(cd "$(dirname "$DAVY_BIN")" && pwd)/$(basename "$DAVY_BIN")"
else
  cargo build --release -j 4
  BIN="$ROOT/target/release/davy"
fi
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
"$BIN" setup --data "$SCRATCH/data" --public-url "$URL" --listen "127.0.0.1:$PORT" > "$SCRATCH/setup.json"
"$BIN" connect e2e-connect --sdk node --local --data "$SCRATCH/data" > "$SCRATCH/connect.json"
python3 - "$SCRATCH/connect.json" <<'PYCONNECT' | sh
import json,sys
print(json.load(open(sys.argv[1]))['test_event'])
PYCONNECT
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
DSN=$("$BIN" project create feedback --data "$SCRATCH/data" --public-url "$URL")
node "$ROOT/e2e/send-feedback.mjs" "$DSN"
for i in {1..50}; do
  "$BIN" sql --data "$SCRATCH/data" --json "SELECT r.reason, r.category, r.quantity FROM client_reports r JOIN projects p ON p.id = r.project_id WHERE p.slug = 'feedback'" > "$SCRATCH/reports.json"
  if grep -q before_send "$SCRATCH/reports.json"; then break; fi
  sleep 0.2
done
ISSUE=$("$BIN" sql --data "$SCRATCH/data" "SELECT i.id FROM issues i JOIN projects p ON p.id = i.project_id WHERE p.slug = 'feedback'" | tail -n 1)
"$BIN" context "$ISSUE" --data "$SCRATCH/data" > "$SCRATCH/context.json"
python3 - "$SCRATCH" <<'PY'
import json,pathlib,sys
p=pathlib.Path(sys.argv[1])
assert json.loads((p/'reports.json').read_text())['rows']==[['before_send','error',1]],(p/'reports.json').read_text()
ctx=json.loads((p/'context.json').read_text())
assert ctx['issue']['title']=='TypeError: Checkout failed',ctx['issue']
fb=[{k:f[k] for k in ('message','name','email','url')} for f in ctx['feedback']]
assert fb==[{'message':'The pay button does nothing','name':'Ana','email':'ana@example.com','url':'https://shop.example.com/cart'}],ctx['feedback']
print('client reports and feedback: verified before_send/error 1, feedback linked to its issue')
PY
DSN=$("$BIN" project create health --data "$SCRATCH/data" --public-url "$URL")
node "$ROOT/e2e/send-sessions.mjs" "$DSN"
for i in {1..50}; do
  "$BIN" sql --data "$SCRATCH/data" --json "SELECT s.release, s.environment, sum(s.started), sum(s.errored), sum(s.crashed), sum(s.abnormal) FROM release_sessions s JOIN projects p ON p.id = s.project_id WHERE p.slug = 'health' GROUP BY 1, 2" > "$SCRATCH/sessions.json"
  "$BIN" sql --data "$SCRATCH/data" --json "SELECT u.flags FROM release_users u JOIN projects p ON p.id = u.project_id WHERE p.slug = 'health' ORDER BY 1" > "$SCRATCH/users.json"
  if grep -q '"rows":\[\["e2e@2","e2e",6,3,1,0\]\]' "$SCRATCH/sessions.json"; then break; fi
  sleep 0.2
done
python3 - "$SCRATCH" <<'PY'
import json,pathlib,sys
p=pathlib.Path(sys.argv[1])
assert json.loads((p/'sessions.json').read_text())['rows']==[['e2e@2','e2e',6,3,1,0]],(p/'sessions.json').read_text()
assert json.loads((p/'users.json').read_text())['rows']==[[2],[6]],(p/'users.json').read_text()
print('release health: verified 6 sessions, 3 errored, 1 crashed, 2 users of whom 1 crashed')
PY
for SDK in sentry-node-7 sentry-node-10; do
  P="checkins-${SDK#sentry-node-}"
  DSN=$("$BIN" project create "$P" --data "$SCRATCH/data" --public-url "$URL")
  "$BIN" monitor add "$P" backup --heartbeat --every 1d --slug backup --data "$SCRATCH/data" > /dev/null
  node "$ROOT/e2e/send-checkins.mjs" "$SDK" "$DSN"
  "$BIN" sql --data "$SCRATCH/data" --json "SELECT r.reason, r.category, r.quantity FROM client_reports r JOIN projects p ON p.id = r.project_id WHERE p.slug = '$P' AND r.category = 'monitor'" > "$SCRATCH/dropped.json"
  "$BIN" sql --data "$SCRATCH/data" --json "SELECT m.slug, c.ok, c.ms, c.reason FROM checks c JOIN monitors m ON m.id = c.monitor_id JOIN projects p ON p.id = m.project_id WHERE p.slug = '$P' ORDER BY m.slug" > "$SCRATCH/checks.json"
  "$BIN" monitor list --data "$SCRATCH/data" --json > "$SCRATCH/monitors.json"
  python3 - "$SCRATCH" "$P" <<'PY'
import json,pathlib,sys
p,project=pathlib.Path(sys.argv[1]),sys.argv[2]
monitors={m['slug']:m for m in json.loads((p/'monitors.json').read_text()) if m['project']==project}
got={s:(m['source'],m['schedule'],m['state'],m['open_since'] is not None) for s,m in monitors.items()}
assert got=={
  'backup':('cli','every 1d','up',False),
  'billing-sync':('sdk','every 30m','up',False),
  'import-feed':('sdk','every 2h','down',True),
  'nightly-report':('sdk','cron 0 * * * *, Europe/Prague','up',False),
},got
assert monitors['nightly-report']['target']=='check-in nightly-report',monitors['nightly-report']
assert monitors['backup']['target'].startswith('/api/heartbeat/'),monitors['backup']
checks=json.loads((p/'checks.json').read_text())['rows']
by={slug:(ok,reason) for slug,ok,ms,reason in checks}
assert by=={'backup':(1,None),'billing-sync':(1,None),'import-feed':(0,'failure ping'),'nightly-report':(1,None)},checks
assert [ms for slug,ok,ms,reason in checks if slug=='billing-sync']==[12500],checks
assert json.loads((p/'dropped.json').read_text())['rows']==[['davy_unknown_monitor','monitor',1]],(p/'dropped.json').read_text()
print(project+': verified 3 monitors made by check-ins, the CLI monitor unchanged, a failure ping and an unknown slug counted')
PY
done
"$BIN" project create edit-from --data "$SCRATCH/data" --public-url "$URL" > /dev/null
"$BIN" project create edit-to --data "$SCRATCH/data" --public-url "$URL" > /dev/null
ADDED=$("$BIN" monitor add edit-from nightly --heartbeat --every 1h --data "$SCRATCH/data")
ID="${ADDED%%	*}"; PING="${ADDED#*	}"
pings() { "$BIN" sql --data "$SCRATCH/data" --json "SELECT count(*) FROM checks WHERE monitor_id = $ID" | python3 -c 'import json,sys; print(json.load(sys.stdin)["rows"][0][0])'; }
[ "$(curl -fsS "$URL$PING")" = ok ]
for i in {1..50}; do [ "$(pings)" = 1 ] && break; sleep 0.1; done
EDITED=$("$BIN" monitor edit "$ID" --every 2h --project edit-to --name 'nightly backup' --data "$SCRATCH/data")
[ "$EDITED" = "$ADDED" ] || { echo "edit printed $EDITED, add printed $ADDED"; exit 1; }
if "$BIN" monitor edit "$ID" --url https://shop.example/ --data "$SCRATCH/data" 2> "$SCRATCH/kind.txt"; then echo 'a heartbeat took a URL'; exit 1; fi
[ "$(curl -fsS "$URL$PING")" = ok ]
for i in {1..50}; do [ "$(pings)" = 2 ] && break; sleep 0.1; done
"$BIN" monitor edit "$ID" --grace 1m --data "$SCRATCH/data" --json > "$SCRATCH/edited.json"
python3 - "$SCRATCH" "$PING" "$(pings)" <<'PY'
import json,pathlib,sys
p,ping,pings=pathlib.Path(sys.argv[1]),sys.argv[2],sys.argv[3]
m=json.loads((p/'edited.json').read_text())
got=(m['project'],m['name'],m['schedule'],m['target'],m['state'],m['source'],pings)
assert got==('edit-to','nightly backup','every 2h',ping,'up','cli','2'),got
assert 'keeps its kind' in (p/'kind.txt').read_text(),(p/'kind.txt').read_text()
print('monitor edit: verified a heartbeat moved to another project with a new schedule keeps its ID, token and 2 checks, and refuses a URL')
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
# The web UI test resolves the sentry-node-7 TypeError. The steps below use
# the other three issues.
(cd "$ROOT/e2e" && pnpm exec playwright install chromium --only-shell && DAVY_BIN="$BIN" DAVY_URL="$URL" DAVY_DATA="$SCRATCH/data" pnpm exec playwright test ui.spec.ts --reporter=line --output "$SCRATCH/playwright")
echo 'web UI: verified sign-in, the list, the in-app frame, resolve, login, logs search, zoom and live, uptime, alerts, projects, setup, a password change, axe and CSP'
# A Vercel Log Drain: the project key alone lets a drain in until the
# project has the drain secret, then only a body signed with it.
DSN=$("$BIN" project create vercel --data "$SCRATCH/data" --public-url "$URL")
VKEY="${DSN#*://}"; VKEY="${VKEY%%@*}"
DRAIN_SECRET=$(python3 -c 'import secrets; print(secrets.token_urlsafe(24))')
export DRAIN_SECRET
NOW_MS=$(python3 -c 'import time; print(int(time.time()*1000))')
drain_body() {
  printf '[{"id":"%s","deploymentId":"dpl_e2e","source":"lambda","host":"shop-e2e.vercel.app","timestamp":%s,"projectId":"prj_e2e","projectName":"shop","level":"%s","message":"%s","statusCode":%s,"requestId":"req-%s","traceId":"1b02cd14bb8642fd092bc23f54c7ffcd","spanId":"f24e8631bd11faa7","proxy":{"method":"GET","path":"/api/cart","statusCode":%s,"clientIp":"120.75.16.101"}}]' "$1" "$NOW_MS" "$2" "$3" "$4" "$1" "$4"
}
drain_sign() {
  python3 -c 'import hashlib,hmac,os,sys; print(hmac.new(os.environ["DRAIN_SECRET"].encode(), sys.stdin.buffer.read(), hashlib.sha1).hexdigest())'
}
drain_post() { # body, then the signature header if any
  curl -s -o /dev/null -w '%{http_code}' -H "Authorization: Bearer $VKEY" -H 'Content-Type: application/json' ${2:+-H "x-vercel-signature: $2"} --data-binary "$1" "$URL/vercel/logs"
}
BODY=$(drain_body 1 info 'vercel unsigned before the secret' 200)
STATUS=$(drain_post "$BODY")
[ "$STATUS" = 200 ] || { echo "vercel drain without a secret gave $STATUS"; exit 1; }
printf '%s\n' "$DRAIN_SECRET" | "$BIN" project vercel vercel --data "$SCRATCH/data" --secret-stdin > "$SCRATCH/vercel.json"
[ "$(cat "$SCRATCH/vercel.json")" = '{"project":"vercel","signature_required":true}' ] || { echo "davy project vercel printed $(cat "$SCRATCH/vercel.json")"; exit 1; }
BODY=$(drain_body 2 error 'vercel signed checkout failed' 500)
SIGNATURE=$(printf '%s' "$BODY" | drain_sign)
WRONG=$(printf '%s' "$BODY" | DRAIN_SECRET=another drain_sign)
for sig in '' "$WRONG"; do
  STATUS=$(drain_post "$BODY" "$sig")
  [ "$STATUS" = 403 ] || { echo "vercel drain signed '$sig' gave $STATUS"; exit 1; }
done
STATUS=$(drain_post "$BODY" "$SIGNATURE")
[ "$STATUS" = 200 ] || { echo "signed vercel drain gave $STATUS"; exit 1; }
for i in {1..50}; do
  "$BIN" logs --data "$SCRATCH/data" --project vercel --since 10m --json > "$SCRATCH/vercel.ndjson"
  if [ "$(wc -l < "$SCRATCH/vercel.ndjson")" -ge 2 ]; then break; fi
  sleep 0.2
done
"$BIN" logs --data "$SCRATCH/data" --project vercel --since 10m --json 'attributes.statusCode:500 AND attributes.source:lambda' > "$SCRATCH/vercel-500.ndjson"
python3 - "$SCRATCH" <<'PY'
import json,os,pathlib,sys
p=pathlib.Path(sys.argv[1])
rows=[json.loads(l) for l in (p/'vercel.ndjson').read_text().splitlines()]
# Both records carry the same timestamp, so their order is not asserted.
got=sorted((r['source'],r['level'],r['message'],r['service']) for r in rows)
assert got==[('vercel','error','vercel signed checkout failed','shop'),('vercel','info','vercel unsigned before the secret','shop')],got
signed=next(r for r in rows if r['level']=='error')
assert (signed['trace_id'],signed['span_id'])==('1b02cd14bb8642fd092bc23f54c7ffcd','f24e8631bd11faa7'),signed
assert signed['attributes']['proxy']['clientIp']=='[Filtered: ip]',signed['attributes']
assert signed['attributes']['requestId']=='req-2',signed['attributes']
found=[json.loads(l)['message'] for l in (p/'vercel-500.ndjson').read_text().splitlines()]
assert found==['vercel signed checkout failed'],found
secret=os.environ['DRAIN_SECRET']
for f in ('server.log','vercel.json'):
  assert secret not in (p/f).read_text(),f'{f} holds the drain secret'
print('vercel: verified a drain with the key alone, then 403 unsigned and wrongly signed, a signed batch searchable by attribute, the client IP scrubbed and the secret in no output')
PY
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
SDK_ISSUE=$("$BIN" sql --data "$SCRATCH/data" "SELECT i.id FROM issues i JOIN projects p ON p.id = i.project_id WHERE p.slug = 'sentry-node-7' AND i.title LIKE 'TypeError%'" | tail -n 1)
"$BIN" event "$SDK_ISSUE" --data "$SCRATCH/data" > "$SCRATCH/payload.json"
python3 - "$SCRATCH/payload.json" <<'PY'
import json,sys
p=json.load(open(sys.argv[1])); assert p['exception']['values'][-1]['type'] in ('TypeError','RangeError')
print('event CLI: verified stored JSON payload')
PY
STATUS=$(curl -s -o /dev/null -w '%{http_code}' -H 'Content-Type: application/json' --data '{}' "$URL/mcp")
[ "$STATUS" = 401 ] || { echo "MCP without token gave $STATUS"; exit 1; }
node "$ROOT/e2e/mcp.mjs" "$URL" "$TOKEN"
"$BIN" feedback --data "$SCRATCH/data" | grep -qx '  tried: e2e tried' || { echo 'davy feedback does not list the MCP report'; exit 1; }
"$BIN" usage --data "$SCRATCH/data" --project e2e-connect --json > "$SCRATCH/usage.json"
python3 - "$SCRATCH/usage.json" <<'PY'
import json,sys
u=json.load(open(sys.argv[1]))
[p]=u['projects']
assert (p['project'],p['logs_days'],p['signals']['errors']['records'])==('e2e-connect',90,1),p
assert isinstance(u['disk']['total'],int) and u['disk']['total']>u['disk']['free']>0,u['disk']
assert u['basis'] is None and u['no_forecast'].startswith('Davy has counted raw log bytes for '),(u['basis'],u.get('no_forecast'))
assert u['estimate'] is None and u['disk']['beyond_10_years'] is None,(u['estimate'],u['disk'])
print(f"usage CLI: next to davy serve, 1 error of {p['signals']['errors']['bytes']} bytes for e2e-connect")
PY
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
OPEN=$(find "$SCRATCH/data" \( -perm -004 -o -perm -040 \) -print)
[ -z "$OPEN" ] || { echo "readable by other users: $OPEN"; exit 1; }
echo 'data directory: no file is readable by other users'
echo 'PASS: all end-to-end assertions passed'

