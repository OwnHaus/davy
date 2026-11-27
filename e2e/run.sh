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
# No log has arrived yet, so logs add nothing and the forecast rests on
# the errors. Ingest counts reach usage_daily on the next flush, every 5 s.
three() { python3 -c 'import json,sys; b=json.load(open(sys.argv[1]))["basis"]; sys.exit(not (b and b["errors"] and b["errors"]["projects"]==3))' "$SCRATCH/errors-only.json"; }
for i in {1..100}; do
  "$BIN" usage --data "$SCRATCH/data" --json > "$SCRATCH/errors-only.json"
  three && break
  sleep 0.2
done
three || { echo "davy usage held no errors of 3 projects after 100 tries: $(cat "$SCRATCH/errors-only.json")"; exit 1; }
python3 - "$SCRATCH/errors-only.json" <<'PY'
import json,sys
u=json.load(open(sys.argv[1]))
b=u['basis']
assert b is not None and u.get('no_forecast') is None,u.get('no_forecast')
assert (b['logs']['basis'],b['confidence'],u['growth_per_day'])==('errors_only','low',0),b
assert b['why'].startswith('Davy counted no raw log bytes in the last 30 days and the disk holds no closed log day since '),b['why']
assert b['why'].endswith("; confidence is low as errors that began less than a whole day ago add over 5% of the errors' growth, so it rests on hours"),b['why']
assert b['errors']['projects']==3 and b['errors']['growth_per_day']>0,b['errors']
assert u['estimate'] is not None and u['disk']['steady_state']>0,(u['estimate'],u['disk'])
print(f"usage CLI: errors alone forecast a server without logs, {b['errors']['events_per_day']} events a day")
PY
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
# The log writer counts raw bytes in log_days once their commit holds; until
# then the server reads as one without logs.
counted() { [ "$("$BIN" sql --data "$SCRATCH/data" "SELECT count(*) FROM log_days WHERE bytes > 0" | tail -n 1)" != 0 ]; }
for i in {1..50}; do
  counted && break
  sleep 0.2
done
counted || { echo 'the log writer counted no raw log bytes in log_days after 50 tries'; exit 1; }
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
echo 'web UI: verified sign-in, the list, the in-app frame, resolve, login, logs search, zoom and live, uptime, alerts, projects, usage, setup, a password change, axe and CSP'
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
# Quotas: a project past its daily quota of 2 error events gets 429 with an
# SDK rate limit entry until midnight UTC, the refusal is counted as
# davy_quota in davy usage, and another project is still accepted. The
# server reads the setting within 5 s and writes refusals on its 5 s flush,
# so both are polled with a deadline. The quota and its count start again
# at midnight UTC, and davy doctor checks the quota later in this run, so a
# run with less than 10 minutes of the UTC day left waits for the next day.
LEFT=$(( 86400 - $(date -u +%s) % 86400 ))
if [ "$LEFT" -lt 600 ]; then echo "quota: waiting $((LEFT + 5)) s for the next UTC day"; sleep $((LEFT + 5)); fi
DSN=$("$BIN" project create capped --data "$SCRATCH/data" --public-url "$URL")
CAPPED="${DSN##*/}"; CAPPED_KEY="${DSN#*://}"; CAPPED_KEY="${CAPPED_KEY%%@*}"
DSN=$("$BIN" project create uncapped --data "$SCRATCH/data" --public-url "$URL")
FREE="${DSN##*/}"; FREE_KEY="${DSN#*://}"; FREE_KEY="${FREE_KEY%%@*}"
"$BIN" project quota capped --errors 2 --data "$SCRATCH/data" > "$SCRATCH/quota.json"
python3 - "$URL" "$CAPPED" "$CAPPED_KEY" "$FREE" "$FREE_KEY" <<'PY'
import json,re,sys,time,urllib.error,urllib.request
url,capped,capped_key,free,free_key=sys.argv[1:]
def store(project,key,i):
  body=json.dumps({'event_id':f'{i:032x}','message':'quota e2e'}).encode()
  req=urllib.request.Request(f'{url}/api/{project}/store/',body,{'Content-Type':'application/json','X-Sentry-Auth':f'Sentry sentry_key={key}, sentry_version=7'})
  try:
    with urllib.request.urlopen(req) as r: return r.status,r.headers
  except urllib.error.HTTPError as e: return e.code,e.headers
deadline=time.monotonic()+30
i=0
while True:
  i+=1
  status,headers=store(capped,capped_key,i)
  if status==429: break
  assert status==200,status
  assert time.monotonic()<deadline,f'no 429 after {i} events in 30 s'
  time.sleep(0.2)
retry=headers['Retry-After']
assert retry.isdigit() and 1<=int(retry)<=300,retry
limits=headers['X-Sentry-Rate-Limits'].split(', ')
assert f'{retry}:error:project:davy_quota' in limits and len(limits)==2,limits
status,_=store(free,free_key,1)
assert status==200,status
print(f'quota: verified 429 with Retry-After {retry} and a davy_quota entry after {i} events, another project still 200')
PY
refused() { "$BIN" usage --data "$SCRATCH/data" --project capped --json | python3 -c 'import json,sys; l=json.load(sys.stdin)["projects"][0]["limits"]; print(l["refused_today"]["davy_quota"], l["errors"]["quota"])'; }
for i in {1..300}; do read -r N Q <<< "$(refused)"; [ "$N" -ge 1 ] && break; sleep 0.1; done
[ "$N" -ge 1 ] || { echo "davy usage counted no davy_quota refusal of capped after 30 s"; exit 1; }
[ "$Q" = 2 ] || { echo "davy usage shows an errors quota of $Q for capped, not 2"; exit 1; }
echo "quota: verified $N davy_quota refusals and the quota of 2 in davy usage --json"
# Savings and discard: 120 events of one issue make it noisy; a person
# discards it at the CLI, and its next events are counted, not stored.
DSN=$("$BIN" project create noisy --data "$SCRATCH/data" --public-url "$URL")
KEY="${DSN#*://}"; KEY="${KEY%%@*}"
PROJECT="${DSN##*/}"
noisy() {
  python3 - "$URL/api/$PROJECT/store/" "$KEY" "$1" "$2" <<'PY'
import json,sys,urllib.request
url,key,n,batch=sys.argv[1],sys.argv[2],int(sys.argv[3]),int(sys.argv[4])
for i in range(n):
  # A payload field named discarded_at is payload: ingest never sets the mark.
  body=json.dumps({'event_id':f'{batch:02x}{i:030x}','message':'noisy checkout timeout','discarded_at':'2026-01-01T00:00:00Z'}).encode()
  req=urllib.request.Request(url,body,{'Content-Type':'application/json','X-Sentry-Auth':f'Sentry sentry_key={key}, sentry_version=7'})
  urllib.request.urlopen(req).read()
PY
}
noisy_count() { "$BIN" issues --data "$SCRATCH/data" --project noisy --json | python3 -c 'import json,sys; print(sum(i["count"] for i in json.load(sys.stdin)))'; }
until_count() {
  for i in {1..100}; do [ "$(noisy_count)" = "$1" ] && return 0; sleep 0.1; done
  echo "noisy holds $(noisy_count) events, not $1"; exit 1
}
noisy 120 1
until_count 120
NOISY=$("$BIN" sql --data "$SCRATCH/data" "SELECT i.id FROM issues i JOIN projects p ON p.id = i.project_id WHERE p.slug = 'noisy'" | tail -n 1)
"$BIN" usage --data "$SCRATCH/data" --project noisy --view savings --json > "$SCRATCH/savings.json"
"$BIN" usage --data "$SCRATCH/data" --project noisy --view savings > "$SCRATCH/savings.txt"
"$BIN" sql --data "$SCRATCH/data" --json "SELECT discarded_at FROM issues WHERE id = $NOISY" > "$SCRATCH/mark.json"
python3 - "$SCRATCH" "$NOISY" <<'PY'
import json,pathlib,shlex,sys
p,issue=pathlib.Path(sys.argv[1]),int(sys.argv[2])
data=shlex.quote(str(p/'data'))
u=json.loads((p/'savings.json').read_text())
# No logs, no pattern; searches recorded for minutes suggest no retention.
assert u['savings']==[{'kind':'issue','id':issue,'project':'noisy','title':'noisy checkout timeout','events':120,'share':1.0,'suggest':f'fix it, or a person can discard it, which also stops storing its real events: davy issue discard {issue} --data {data}','command':f'davy issue discard {issue} --data {data}','try':f'issue_context {issue}'}],u
assert u['note'].startswith('last_queried is approximate: '),u['note']
text=(p/'savings.txt').read_text()
assert f'issue     {issue} in noisy: 120 events, 100% of its error events\n          noisy checkout timeout\n          fix it, or a person can discard it, which also stops storing its real events: davy issue discard {issue} --data {data}\n          try: issue_context {issue}\n' in text,text
assert json.loads((p/'mark.json').read_text())['rows']==[[None]],(p/'mark.json').read_text()
print(f'savings CLI: issue {issue} sends 120 of 120 error events in noisy, with the discard command; a payload field did not discard it')
PY
"$BIN" issue discard "$NOISY" --data "$SCRATCH/data" > "$SCRATCH/discard.json"
noisy 5 2
discarded() { "$BIN" usage --data "$SCRATCH/data" --project noisy --json | python3 -c 'import json,sys; print(json.load(sys.stdin)["projects"][0]["errors_discarded"])'; }
for i in {1..100}; do [ "$(discarded)" = 5 ] && break; sleep 0.1; done
DISCARDED=$(discarded)
STORED=$(noisy_count)
"$BIN" usage --data "$SCRATCH/data" --project noisy --view savings --json > "$SCRATCH/savings.json"
"$BIN" issue discard "$NOISY" --undo --data "$SCRATCH/data" > "$SCRATCH/undo.json"
noisy 1 3
until_count 121
python3 - "$SCRATCH" "$NOISY" "$DISCARDED" "$STORED" <<'PY'
import json,pathlib,sys
p,issue=pathlib.Path(sys.argv[1]),int(sys.argv[2])
d=json.loads((p/'discard.json').read_text())
assert [d[k] for k in ('issue','project','title','discarded')]==[issue,'noisy','noisy checkout timeout',True],d
assert (sys.argv[3],sys.argv[4])==('5','120'),sys.argv[3:]
# A discarded issue is no longer suggested.
assert json.loads((p/'savings.json').read_text())['savings']==[],(p/'savings.json').read_text()
undo=json.loads((p/'undo.json').read_text())
assert (undo['discarded'],undo['discarded_at'])==(False,None),undo
print(f'discard CLI: 5 events of discarded issue {issue} counted, not stored; after --undo the next is stored (121)')
PY
# Discarded again, the issue says so in every list, and mcp.mjs reads the
# mark through issue_context.
"$BIN" issue discard "$NOISY" --data "$SCRATCH/data" > /dev/null
"$BIN" issues --data "$SCRATCH/data" --project noisy --json > "$SCRATCH/issues.json"
"$BIN" issues --data "$SCRATCH/data" --discarded --json > "$SCRATCH/discarded.json"
"$BIN" issues --data "$SCRATCH/data" --project noisy > "$SCRATCH/issues.txt"
"$BIN" context "$NOISY" --data "$SCRATCH/data" > "$SCRATCH/context.json"
python3 - "$SCRATCH" "$NOISY" <<'PY'
import json,pathlib,re,sys
p,issue=pathlib.Path(sys.argv[1]),int(sys.argv[2])
[i]=json.loads((p/'issues.json').read_text())
assert i['id']==issue and re.fullmatch(r'\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(\.\d+)?Z',i['discarded_at'] or ''),i
assert [d['id'] for d in json.loads((p/'discarded.json').read_text())]==[issue],(p/'discarded.json').read_text()
text=(p/'issues.txt').read_text()
assert text.startswith(f'{issue}\tunresolved (discarded)\t121\tnoisy checkout timeout'),text
c=json.loads((p/'context.json').read_text())
assert c['issue']['discarded_at']==i['discarded_at'],c['issue']
print(f'discard mark: issue {issue} shows discarded_at {i["discarded_at"]} in davy issues, --discarded and davy context')
PY
# Retention fit: project -x has 8 days of recorded searches that read a day
# back and 2 MB of logs 20 days old, which a shorter retention deletes.
"$BIN" project create --data "$SCRATCH/data" --public-url "$URL" -- -x > /dev/null
python3 - "$SCRATCH/data/davy.sqlite" <<'PY'
import datetime,sqlite3,sys
c=sqlite3.connect(sys.argv[1],timeout=10)
[(pid,)]=c.execute("SELECT id FROM projects WHERE slug='-x'").fetchall()
today=datetime.datetime.now(datetime.timezone.utc).date()
day=lambda n:(today-datetime.timedelta(days=n)).isoformat()
with c:
  c.executemany("INSERT INTO search_days(day,project_id,searches,oldest_read_days) VALUES (?,?,1,1)",[(day(n),pid) for n in range(1,9)])
  c.execute("INSERT INTO log_days(day,project_id,records,bytes,cut) VALUES (?,?,1,2000000,0)",(day(20),pid))
c.close()
PY
"$BIN" usage --data "$SCRATCH/data" --project=-x --view savings --json > "$SCRATCH/fit.json"
"$BIN" usage --data "$SCRATCH/data" --project=-x --view savings > "$SCRATCH/fit.txt"
python3 - "$SCRATCH" "$BIN" <<'PY'
import json,pathlib,re,shlex,sys
p,binary=pathlib.Path(sys.argv[1]),sys.argv[2]
data=shlex.quote(str(p/'data'))
[r]=json.loads((p/'fit.json').read_text())['savings']
n,days=r['logs_days'],int(r['suggest'].split()[1])
assert (r['kind'],r['project'],r['history_days'],days<n,r['saves']>0)==('retention','-x',7,True,True),r
assert r['suggest']==f'keep {days} days; a person can run: davy project retention --logs {days} --data {data} -- -x',r
assert r['command']==f'davy project retention --logs {days} --data {data} -- -x',r
# try reads: it counts the logs the cut would delete.
assert r['try']==f'count_logs project="-x" since={n}d until={days}d',r
text=(p/'fit.txt').read_text()
assert f'          a person can run: davy project retention --logs {days} --data {data} -- -x\n          try: count_logs project="-x" since={n}d until={days}d\n' in text,text
(p/'retention-x.sh').write_text(shlex.quote(binary)+r['command'].removeprefix('davy')+'\n')
print(f'retention fit CLI: -x keeps {n} days, suggests {days} with a read-only try and the command for a person')
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
node "$ROOT/e2e/mcp.mjs" "$URL" "$TOKEN" "$NOISY"
# The command the view printed for -x works as printed, slug and all.
sh "$SCRATCH/retention-x.sh" > "$SCRATCH/retention-x.json"
"$BIN" usage --data "$SCRATCH/data" --project=-x --view savings --json > "$SCRATCH/fit.json"
python3 - "$SCRATCH" <<'PY'
import json,pathlib,sys
p=pathlib.Path(sys.argv[1])
set_=json.loads((p/'retention-x.json').read_text())
assert (set_['project'],set_['errors_days'])==('-x',None) and set_['logs_days']<30,set_
assert json.loads((p/'fit.json').read_text())['savings']==[],(p/'fit.json').read_text()
print(f'retention command: davy project retention -- -x set logs to {set_["logs_days"]} days; the view suggests nothing more')
PY
node "$ROOT/e2e/drops.mjs" "$BIN" "$SCRATCH/data" "$URL" "$TOKEN"
"$BIN" feedback --data "$SCRATCH/data" | grep -qx '  tried: e2e tried' || { echo 'davy feedback does not list the MCP report'; exit 1; }
"$BIN" usage --data "$SCRATCH/data" --project e2e-connect --json > "$SCRATCH/usage.json"
python3 - "$SCRATCH/usage.json" <<'PY'
import json,sys
u=json.load(open(sys.argv[1]))
[p]=u['projects']
assert (p['project'],p['logs_days'],p['signals']['errors']['records'])==('e2e-connect',90,1),p
assert isinstance(u['disk']['total'],int) and u['disk']['total']>u['disk']['free']>0,u['disk']
# Logs counted for minutes have no log basis yet: they count as 0 and the
# forecast rests on the errors, at low confidence.
b=u['basis']
assert b is not None and u.get('no_forecast') is None,u.get('no_forecast')
assert (b['logs']['basis'],b['confidence'],u['growth_per_day'])==('errors_only','low',0),b
assert b['why'].startswith('logs are not in this forecast yet, so it rests on errors alone and counts logs as 0: Davy has counted raw log bytes for '),b['why']
assert b['why'].endswith('; confidence is low while logs are left out'),b['why']
# Whether the disk fills, and when, depends on the runner's disk and on
# which usage flushes ran by now, so only the shape is asserted.
d=u['disk']
assert u['estimate'] is not None and isinstance(d['beyond_10_years'],bool),(u['estimate'],d)
assert not (d['beyond_10_years'] and d['full_on'] is not None),d
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
# Davy monitors itself. With the server stopped, a panic is saved as a crash
# file and a stray file in the log root makes the log writer warn at its next
# start. The restarted server records both in Davy's own project, and a panic
# while it runs is recorded within its 5 s scan. Each panic runs from another
# directory than the build's, so its frames name Davy's files from the crate
# root wherever Davy runs.
kill "$PID"; wait "$PID" || true; PID=''
CRASH_STATUS=0; (cd "$SCRATCH" && "$BIN" crash --data "$SCRATCH/data") 2>"$SCRATCH/crash.log" || CRASH_STATUS=$?
[ "$CRASH_STATUS" = 101 ] || { echo "davy crash exited with $CRASH_STATUS"; cat "$SCRATCH/crash.log"; exit 1; }
grep -q 'the panic is saved in' "$SCRATCH/crash.log" || { echo 'davy crash did not save the panic'; cat "$SCRATCH/crash.log"; exit 1; }
grep -q 'this panic is a test' "$SCRATCH/crash.log" || { echo 'davy crash did not say the panic is a test'; cat "$SCRATCH/crash.log"; exit 1; }
python3 - "$SCRATCH/data/crashes" <<'PY'
import os,stat,sys
d=sys.argv[1]; [f]=os.listdir(d)
modes=(oct(stat.S_IMODE(os.stat(d).st_mode)),oct(stat.S_IMODE(os.stat(os.path.join(d,f)).st_mode)))
assert modes==('0o700','0o600'),modes
assert f.endswith('.json') and not f.startswith('.'),f
print('self report: davy crash saved one crash file, mode 0600 in a directory of mode 0700')
PY
(umask 077; : > "$SCRATCH/data/logs/2020-01-01")
DAVY_TOKEN="$TOKEN" "$BIN" serve --data "$SCRATCH/data" --listen "127.0.0.1:$PORT" --public-url "$URL" >>"$SCRATCH/server.log" 2>&1 &
PID=$!
for i in {1..100}; do
  if curl -s -o /dev/null "$URL/"; then break; fi
  sleep 0.1
done
for i in {1..100}; do
  OWN=$("$BIN" sql "SELECT slug FROM projects WHERE own=1" --data "$SCRATCH/data" --json 2>/dev/null | python3 -c 'import json,sys; rows=json.load(sys.stdin)["rows"]; print(rows[0][0] if rows else "")' || true)
  [ -n "$OWN" ] || { sleep 0.1; continue; }
  "$BIN" issues --data "$SCRATCH/data" --project "$OWN" --json > "$SCRATCH/davy-issues.json"
  "$BIN" logs --data "$SCRATCH/data" --project "$OWN" --since 10m --level warn --json > "$SCRATCH/davy-logs.ndjson"
  if grep -q 'a test panic' "$SCRATCH/davy-issues.json" && grep -q '2020-01-01' "$SCRATCH/davy-logs.ndjson"; then break; fi
  sleep 0.1
done
[ "$OWN" = davy ] || { echo "Davy's own project is \"$OWN\", not davy"; exit 1; }
[ -z "$(ls -A "$SCRATCH/data/crashes")" ] || { echo 'the crash file was not recorded at start'; exit 1; }
(cd "$SCRATCH" && "$BIN" crash --data "$SCRATCH/data") 2>>"$SCRATCH/crash.log" || true
for i in {1..100}; do
  "$BIN" issues --data "$SCRATCH/data" --project "$OWN" --json > "$SCRATCH/davy-issues.json"
  if grep -q '"count": *2' "$SCRATCH/davy-issues.json"; then break; fi
  sleep 0.1
done
DAVY_ISSUE=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))[0]["id"])' "$SCRATCH/davy-issues.json")
"$BIN" context "$DAVY_ISSUE" --data "$SCRATCH/data" > "$SCRATCH/davy-context.json"
# Doctor may fail a check of the machine that runs the suite, such as a
# nearly full disk, so it may exit 1, but never because of its davy check.
DOCTOR_STATUS=0; "$BIN" doctor --data "$SCRATCH/data" --json > "$SCRATCH/doctor.json" 2>"$SCRATCH/doctor.log" || DOCTOR_STATUS=$?
case $DOCTOR_STATUS in 0 | 1) ;; *) echo "doctor exited with $DOCTOR_STATUS"; cat "$SCRATCH/doctor.log"; exit 1 ;; esac
python3 - "$SCRATCH" "$DAVY_ISSUE" "$DOCTOR_STATUS" <<'PY'
import json,pathlib,sys
p=pathlib.Path(sys.argv[1]); issue=int(sys.argv[2]); doctor_status=int(sys.argv[3])
issues=json.loads((p/'davy-issues.json').read_text())
assert [(i['title'],i['count']) for i in issues]==[('panic: a test panic, asked for with `davy crash`',2)],issues
lines=[json.loads(l) for l in (p/'davy-logs.ndjson').read_text().splitlines()]
stray=[l for l in lines if '2020-01-01' in l['message']]
assert stray and stray[0]['level']=='warn' and stray[0]['service']=='davy',lines
assert stray[0]['attributes']['component']=='log writer',stray[0]
ctx=json.loads((p/'davy-context.json').read_text())
event=ctx['latest_event']
assert (event['level'],event['platform'])==('fatal','rust'),event
assert event['release'].startswith('davy@'),event['release']
exc=event['exceptions'][-1]
assert (exc['type'],exc['handled'])==('panic',False),exc
top=[f for f in exc['frames'] if f['in_app']][-1]
assert top['function'].split('::')[-1]=='test_panic' and top['filename']=='src/self_report/crash.rs',top
assert isinstance(top['lineno'],int),top
checks=json.loads((p/'doctor.json').read_text())['checks']
failing=[c['name'] for c in checks if c['status']=='fail']
assert (doctor_status==1)==bool(failing),(doctor_status,failing)
assert 'davy' not in failing,checks
[check]=[c for c in checks if c['name']=='davy']
assert check['status']=='warn' and f'issue {issue} ' in check['detail'],check
assert check['fix'].startswith(f'davy context {issue} --data '),check
[quota]=[c for c in checks if c['name']=='quotas']
assert quota['status']=='warn' and quota['detail'].startswith('capped used ') and 'of its 2 errors quota today' in quota['detail'],quota
assert quota['fix'].startswith('davy project quota capped --data '),quota
print(f"self report: a warning of the log writer and a crash recorded at start and live, issue {issue} with 2 events, top frame test_panic at {top['filename']}:{top['lineno']}, and doctor names davy context {issue} and the quota capped reached")
PY
# With --no-self-report Davy records nothing of itself: a crash file of an
# earlier run stays where it is, the log writer's warning about a stray file
# goes to stderr only, and the store holds no project and no log line.
kill "$PID"; wait "$PID" || true; PID=''
QUIET="$SCRATCH/quiet"
(cd "$SCRATCH" && "$BIN" crash --data "$QUIET") 2>/dev/null || true
(umask 077; mkdir -p "$QUIET/logs"; : > "$QUIET/logs/2020-01-01")
"$BIN" serve --data "$QUIET" --listen "127.0.0.1:$PORT" --public-url "$URL" --no-self-report >"$SCRATCH/quiet.log" 2>&1 &
PID=$!
for i in {1..100}; do
  if grep -q '2020-01-01' "$SCRATCH/quiet.log" && curl -s -o /dev/null "$URL/"; then break; fi
  sleep 0.1
done
grep -q '2020-01-01' "$SCRATCH/quiet.log" || { echo 'the log writer did not warn about the stray file'; cat "$SCRATCH/quiet.log"; exit 1; }
QUIET_ROWS=$("$BIN" sql "SELECT (SELECT count(*) FROM projects), (SELECT count(*) FROM log_days), (SELECT count(*) FROM issues)" --data "$QUIET" --json | python3 -c 'import json,sys; print(json.load(sys.stdin)["rows"])')
[ "$QUIET_ROWS" = '[[0, 0, 0]]' ] || { echo "with --no-self-report the store holds projects, log days and issues $QUIET_ROWS"; exit 1; }
[ "$(ls "$QUIET/crashes" | wc -l | tr -d ' ')" = 1 ] || { echo 'with --no-self-report the crash file was taken'; ls -la "$QUIET/crashes"; exit 1; }
kill "$PID"; wait "$PID" || true; PID=''
echo 'self report: with --no-self-report a crash file and a warning of the log writer are not recorded, and no project is made'
# The cold tier: logs for a day three days back, searched hot, then the
# server restarts with --hot-days 1 and the same searches read it cold.
COLD="$SCRATCH/cold"
up() {
  DAVY_TOKEN="$TOKEN" "$BIN" serve --data "$COLD" --listen "127.0.0.1:$PORT" --public-url "$URL" "$@" >>"$SCRATCH/cold.log" 2>&1 &
  PID=$!
  for i in {1..100}; do
    if curl -s -o /dev/null "$URL/"; then break; fi
    sleep 0.1
  done
}
up
node "$ROOT/e2e/cold.mjs" before "$BIN" "$COLD" "$URL" "$TOKEN" "$SCRATCH/cold-before.json"
kill "$PID"; wait "$PID" || true; PID=''
up --hot-days 1
node "$ROOT/e2e/cold.mjs" after "$BIN" "$COLD" "$URL" "$TOKEN" "$SCRATCH/cold-before.json" || { cat "$SCRATCH/cold.log"; exit 1; }
kill "$PID"; wait "$PID" || true; PID=''
OPEN=$(find "$SCRATCH/data" \( -perm -004 -o -perm -040 \) -print)
[ -z "$OPEN" ] || { echo "readable by other users: $OPEN"; exit 1; }
echo 'data directory: no file is readable by other users'
echo 'PASS: all end-to-end assertions passed'

