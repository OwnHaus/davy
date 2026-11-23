#!/bin/bash
# Usage: bench/load-issues.sh [issues] [requests per case]
# Fills a store with issues, three events each, then times GET /ui/api/issues
# for each sort and filter. Run it at normal priority: under `taskpolicy -b`
# the server answered about five times slower.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
ISSUES="${1:-100000}"; N="${2:-200}"
cargo build --release -j 4 --bin davy 2>&1 | tail -1
BIN="$ROOT/target/release/davy"
SCRATCH="$ROOT/.scratch/load-issues-$$"
mkdir -p "$SCRATCH"
PID=''
cleanup() {
  if [ -n "$PID" ]; then kill "$PID" 2>/dev/null || true; wait "$PID" 2>/dev/null || true; fi
  rm -rf "$SCRATCH"
}
trap cleanup EXIT
PORT=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()')
URL="http://127.0.0.1:$PORT"
for P in web api worker mobile admin billing search mail jobs edge; do
  "$BIN" project create "$P" --data "$SCRATCH/data" --public-url "$URL" > /dev/null
done
# Rows go in through SQL, so the store's own triggers fill issues_text and
# issue_tags. Davy never runs ANALYZE, so neither does this.
python3 -I - "$SCRATCH/data/davy.sqlite" "$ISSUES" <<'PY'
import sqlite3, sys, time
c = sqlite3.connect(sys.argv[1]); n = int(sys.argv[2]); t = time.time()
c.executescript(f"""
BEGIN;
CREATE TEMP TABLE n(i INTEGER PRIMARY KEY);
WITH RECURSIVE s(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM s WHERE i<{n}) INSERT INTO n SELECT i FROM s;
INSERT INTO issues(id,project_id,grouping_version,fingerprint,title,culprit,level,status,first_seen,last_seen,event_count)
SELECT i, 1+i%10, 1, 'fp'||i,
  CASE i%4 WHEN 0 THEN 'TypeError: Cannot read properties of undefined (reading ''f'||i||''')'
           WHEN 1 THEN 'RangeError: Maximum call stack size exceeded in step '||i
           WHEN 2 THEN 'Error: request '||i||' to upstream timed out'
           ELSE 'ReferenceError: v'||i||' is not defined' END,
  'fn'||(i%97)||'_mod'||(i%500),
  CASE WHEN i%9=0 THEN 'warning' ELSE 'error' END,
  CASE WHEN i%20=0 THEN 'resolved' WHEN i%20=1 THEN 'ignored' ELSE 'unresolved' END,
  0, CAST(strftime('%s','now') AS INTEGER)*1000 - CASE WHEN abs(random())%5=0 THEN abs(random())%86400000 ELSE abs(random())%(30*86400000) END,
  1+(abs(random())%1000)*(abs(random())%1000)/100
FROM n;
UPDATE issues SET first_seen = last_seen - abs(random())%(60*86400000);
INSERT INTO events(project_id,issue_id,event_id,timestamp,received_at,level,release,environment)
SELECT i.project_id, i.id, printf('%032x', i.id*4+k.k), i.last_seen-k.k*1000, i.last_seen-k.k*1000, i.level,
  'v1.'||((i.id+k.k)%50),
  CASE abs(random())%10 WHEN 0 THEN 'staging' WHEN 1 THEN 'development' ELSE 'production' END
FROM issues i, (SELECT 0 k UNION ALL SELECT 1 UNION ALL SELECT 2) k;
INSERT INTO issue_hourly(issue_id,hour,count)
SELECT i.id, (i.last_seen/3600000 - h.h)*3600000, 1+abs(random())%40
FROM issues i, (SELECT 0 h UNION ALL SELECT 1 UNION ALL SELECT 3 UNION ALL SELECT 7 UNION ALL SELECT 15) h
WHERE i.last_seen > CAST(strftime('%s','now') AS INTEGER)*1000 - 86400000;
COMMIT;
""")
print(f"filled {n} issues and {3*n} events in {time.time()-t:.1f}s")
PY
DAVY_TOKEN=$(python3 -c 'import secrets; print(secrets.token_urlsafe(32))') "$BIN" serve --data "$SCRATCH/data" --listen "127.0.0.1:$PORT" --public-url "$URL" >"$SCRATCH/server.log" 2>&1 &
PID=$!
for i in {1..100}; do curl -s -o /dev/null "$URL/" && break; sleep 0.1; done
LINK=$("$BIN" user add load@example.com --data "$SCRATCH/data" --public-url "$URL" | grep -o 'http[^ ]*/ui/welcome#[^ ]*')
curl -fsS -c "$SCRATCH/jar.txt" -o /dev/null -H "Origin: $URL" -H 'Content-Type: application/json' \
  --data "{\"secret\":\"${LINK#*#}\",\"password\":\"correct horse battery staple\"}" "$URL/ui/api/welcome"
COOKIE=$(sed 's/^#HttpOnly_//' "$SCRATCH/jar.txt" | awk '!/^#/ && NF>=7 {print $6"="$7}' | head -1)
echo "load before: $(sysctl -n vm.loadavg)"
python3 -I - "$PORT" "$COOKIE" "$N" <<'PY'
import http.client, json, statistics, sys, time
port, cookie, n = int(sys.argv[1]), sys.argv[2], int(sys.argv[3])
cases = {
  "default: unresolved, last seen": "status=unresolved",
  "all, last seen": "",
  "unresolved, most events": "status=unresolved&sort=events",
  "unresolved, first seen": "status=unresolved&sort=first_seen",
  "resolved, last seen": "status=resolved",
  "project, unresolved": "project=web&status=unresolved",
  "project, unresolved, most events": "project=web&status=unresolved&sort=events",
  "text, many hits (timed out)": "status=unresolved&q=timed+out",
  "text, one hit (step 99997)": "status=unresolved&q=step+99997",
  "text, no hit": "q=nothing+matches+this",
  "text, two characters (fn)": "status=unresolved&q=fn",
  "text, two characters, no hit (qz)": "q=qz",
  "environment staging": "status=unresolved&environment=staging",
  "release v1.7": "status=unresolved&release=v1.7",
  "environment with no issues": "environment=nowhere",
}
c = http.client.HTTPConnection("127.0.0.1", port)
def get(path):
  c.request("GET", path, headers={"Cookie": cookie})
  r = c.getresponse(); body = r.read()
  assert r.status == 200, (path, r.status, body[:200])
  return json.loads(body)
worst = 0
for name, qs in cases.items():
  path = "/ui/api/issues" + ("?" + qs if qs else "")
  rows = len(get(path)["issues"])
  ts = []
  for _ in range(n):
    t = time.perf_counter(); get(path); ts.append((time.perf_counter() - t) * 1000)
  ts.sort(); p95 = ts[int(len(ts) * 0.95) - 1]; worst = max(worst, p95)
  print(f"{name:34s} rows {rows:3d}  p50 {statistics.median(ts):6.1f} ms  p95 {p95:6.1f} ms  max {ts[-1]:6.1f} ms", flush=True)
print(f"worst p95 {worst:.1f} ms against a budget of 100 ms")
PY
echo "load after: $(sysctl -n vm.loadavg)"
