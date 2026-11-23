#!/bin/bash
# Usage: bench/agent-search.sh [logs] [days] [issues] [requests per case]
# Times what a coding agent asks Davy over MCP when the store holds months of
# data: search_logs over recent and old windows, sql over the events of those
# months, and issue_context. Logs are spread evenly over the last days, and
# each issue has events spread over the same days.
# Each case runs once right after the server starts ("first"), then N times.
# The data was just written, so the OS still caches it. A first call measures
# opening the indexes, not reading a cold disk.
# Run it at normal priority: under `taskpolicy -b` the server answered about
# five times slower.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
LOGS="${1:-18000000}"; DAYS="${2:-90}"; ISSUES="${3:-20000}"; N="${4:-20}"
cargo build --release -j 4 --bin davy --example fill_logs 2>&1 | tail -1
BIN="$ROOT/target/release/davy"
SCRATCH="$ROOT/.scratch/agent-search-$$"
mkdir -p "$SCRATCH"
PID=''
cleanup() {
  if [ -n "$PID" ]; then kill "$PID" 2>/dev/null || true; wait "$PID" 2>/dev/null || true; fi
  rm -rf "$SCRATCH"
}
trap cleanup EXIT
PORT=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()')
URL="http://127.0.0.1:$PORT"
for P in web api worker mobile; do
  "$BIN" project create "$P" --data "$SCRATCH/data" --public-url "$URL" > /dev/null
done
"$ROOT/target/release/examples/fill_logs" "$SCRATCH/data" "$LOGS" "$((DAYS * 24))" 4
# Issues and events go in through SQL, twenty events per issue spread over the
# days, so the store's triggers fill issues_text and issue_tags.
python3 -I - "$SCRATCH/data/davy.sqlite" "$ISSUES" "$DAYS" <<'PY'
import sqlite3, sys, time
c = sqlite3.connect(sys.argv[1]); n, days = int(sys.argv[2]), int(sys.argv[3]); t = time.time()
span = days * 86400000
c.executescript(f"""
BEGIN;
CREATE TEMP TABLE n(i INTEGER PRIMARY KEY);
WITH RECURSIVE s(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM s WHERE i<{n}) INSERT INTO n SELECT i FROM s;
CREATE TEMP TABLE k(k INTEGER PRIMARY KEY);
WITH RECURSIVE s(k) AS (SELECT 0 UNION ALL SELECT k+1 FROM s WHERE k<19) INSERT INTO k SELECT k FROM s;
INSERT INTO issues(id,project_id,grouping_version,fingerprint,title,culprit,level,status,first_seen,last_seen,event_count)
SELECT i, 1+i%4, 1, 'fp'||i,
  CASE i%4 WHEN 0 THEN 'TypeError: Cannot read properties of undefined (reading ''f'||i||''')'
           WHEN 1 THEN 'RangeError: Maximum call stack size exceeded in step '||i
           WHEN 2 THEN 'Error: request '||i||' to upstream timed out'
           ELSE 'ReferenceError: v'||i||' is not defined' END,
  'fn'||(i%97)||'_mod'||(i%500),
  CASE WHEN i%9=0 THEN 'warning' ELSE 'error' END,
  CASE WHEN i%20=0 THEN 'resolved' WHEN i%20=1 THEN 'ignored' ELSE 'unresolved' END,
  0, 0, 20
FROM n;
WITH e AS MATERIALIZED (
  SELECT i.project_id p, i.id id, i.level lv, k.k kk,
    CAST(strftime('%s','now') AS INTEGER)*1000 - {span} + abs(random()) % {span} AS t
  FROM issues i, k)
INSERT INTO events(project_id,issue_id,event_id,timestamp,received_at,level,release,environment)
SELECT p, id, printf('%032x', id*20+kk), t, t, lv, 'v1.'||(kk*5+id%5),
  CASE abs(random())%10 WHEN 0 THEN 'staging' ELSE 'production' END
FROM e;
UPDATE issues SET first_seen=(SELECT min(timestamp) FROM events WHERE issue_id=issues.id),
                  last_seen=(SELECT max(timestamp) FROM events WHERE issue_id=issues.id);
COMMIT;
""")
print(f"filled {n} issues and {20*n} events in {time.time()-t:.1f}s")
PY
echo "log store: $(du -sh "$SCRATCH/data/logs" | cut -f1) in $(ls "$SCRATCH/data/logs" | grep -c '^20') days, sqlite: $(du -sh "$SCRATCH/data/davy.sqlite" | cut -f1)"
echo "files per day, most: $(for d in "$SCRATCH"/data/logs/20*; do ls "$d" | wc -l; done | sort -n | tail -1)"
TOKEN=$(python3 -c 'import secrets; print(secrets.token_urlsafe(32))')
DAVY_TOKEN="$TOKEN" "$BIN" serve --data "$SCRATCH/data" --listen "127.0.0.1:$PORT" --public-url "$URL" \
  --log-retention-days "$((DAYS + 10))" >"$SCRATCH/server.log" 2>&1 &
PID=$!
for i in {1..100}; do curl -s -o /dev/null "$URL/" && break; sleep 0.1; done
echo "load before: $(sysctl -n vm.loadavg)"
python3 -I - "$PORT" "$TOKEN" "$N" "$DAYS" <<'PY'
import http.client, json, statistics, sys, time
port, token, n, days = int(sys.argv[1]), sys.argv[2], int(sys.argv[3]), int(sys.argv[4])
c = http.client.HTTPConnection("127.0.0.1", port)
session = None
def rpc(method, params, id=1):
  global session
  h = {"Authorization": f"Bearer {token}", "Content-Type": "application/json",
       "Accept": "application/json, text/event-stream"}
  if session: h["Mcp-Session-Id"] = session
  body = {"jsonrpc": "2.0", "method": method, "params": params}
  if id is not None: body["id"] = id
  c.request("POST", "/mcp", json.dumps(body), h)
  r = c.getresponse(); raw = r.read()
  assert r.status in (200, 202), (method, r.status, raw[:300])
  session = session or r.getheader("Mcp-Session-Id")
  return json.loads(raw) if raw else None
def call(tool, args):
  out = rpc("tools/call", {"name": tool, "arguments": args})
  assert "result" in out and not out["result"].get("isError"), (tool, args, str(out)[:400])
  return json.loads(out["result"]["content"][0]["text"])
rpc("initialize", {"protocolVersion": "2025-06-18", "capabilities": {},
                   "clientInfo": {"name": "agent-search-bench", "version": "1"}})
rpc("notifications/initialized", {}, id=None)
old = call("search_logs", {"since": f"{days - 1}d", "until": f"{days - 2}d", "limit": 1})[0]
trace = call("search_logs", {"since": "50d", "until": "49d", "query": "request", "limit": 1})[0]["trace_id"]
needle = old["attributes"]["hostname"].split("-")[-1]
req = call("search_logs", {"since": "60d", "until": "59d", "query": "request", "limit": 1})[0]["attributes"]["req"]["id"].split("-")[1]
issue = call("sql", {"query": "SELECT id FROM issues WHERE status='unresolved' ORDER BY last_seen DESC LIMIT 1"})["rows"][0][0]
full = f"{days}d"
cases = [
  ("search_logs", "last hour, newest 100", {"since": "1h"}),
  ("search_logs", "last 24h, text (timeout)", {"since": "24h", "query": "timeout"}),
  ("search_logs", "last 7d, errors", {"since": "7d", "level": "error"}),
  ("search_logs", "30d, attribute (status 500 POST)", {"since": "30d", "query": "attributes.res.statusCode:500 AND attributes.req.method:post"}),
  ("search_logs", f"{full}, one request id (60d old)", {"since": full, "query": req, "limit": 10}),
  ("search_logs", f"{full}, one trace id (50d old)", {"since": full, "query": f"trace_id:{trace}", "limit": 10}),
  ("search_logs", f"{full}, rare host part, 500 hits", {"since": full, "query": needle, "limit": 500}),
  ("search_logs", f"{full}, no hit", {"since": full, "query": "nothingmatchesthis", "limit": 10}),
  ("search_logs", "one old day (80d back)", {"since": "81d", "until": "80d", "level": "warn"}),
  ("sql", "open issues by last seen", {"query": "SELECT id,title,event_count,last_seen FROM issues WHERE status='unresolved' ORDER BY last_seen DESC LIMIT 20"}),
  ("sql", "issue titles LIKE '%timed out%'", {"query": "SELECT id,title FROM issues WHERE title LIKE '%timed out%' LIMIT 50"}),
  ("sql", "one issue, events per day", {"query": f"SELECT timestamp/86400000 AS day, count(*) FROM events WHERE issue_id={issue} GROUP BY day"}),
  ("sql", f"all events per day over {full}", {"query": "SELECT timestamp/86400000 AS day, count(*) FROM events GROUP BY day ORDER BY day"}),
  ("sql", "events per release, production", {"query": "SELECT release, count(*) FROM events WHERE environment='production' GROUP BY release ORDER BY 2 DESC"}),
  ("issue_context", "newest unresolved issue", {"issue_id": issue}),
]
worst = 0
for tool, name, args in cases:
  t = time.perf_counter(); out = call(tool, args); first = (time.perf_counter() - t) * 1000
  rows = len(out) if isinstance(out, list) else len(out["rows"]) if "rows" in out else 1
  ts = []
  for _ in range(n):
    t = time.perf_counter(); call(tool, args); ts.append((time.perf_counter() - t) * 1000)
  ts.sort(); p95 = ts[max(0, int(len(ts) * 0.95) - 1)]; worst = max(worst, p95)
  print(f"{tool:13s} {name:38s} rows {rows:4d}  first {first:8.1f} ms  p50 {statistics.median(ts):8.1f} ms  p95 {p95:8.1f} ms", flush=True)
print(f"worst p95 {worst:.1f} ms")
PY
echo "load after: $(sysctl -n vm.loadavg)"
grep -i "error\|warn" "$SCRATCH/server.log" | head -5 || true
