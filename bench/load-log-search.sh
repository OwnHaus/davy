#!/bin/bash
# Usage: bench/load-log-search.sh [logs] [hours] [requests per case]
# Fills a log store with logs spread over the last hours, then times the
# Logs page routes: a page, a page further back, live, and the histogram.
# Run it at normal priority: under `taskpolicy -b` the server answered
# about five times slower.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
LOGS="${1:-10000000}"; HOURS="${2:-24}"; N="${3:-50}"
cargo build --release -j 4 --bin davy --example fill_logs 2>&1 | tail -1
BIN="$ROOT/target/release/davy"
SCRATCH="$ROOT/.scratch/load-log-search-$$"
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
"$ROOT/target/release/examples/fill_logs" "$SCRATCH/data" "$LOGS" "$HOURS" 4
echo "log store: $(du -sh "$SCRATCH/data/logs" | cut -f1), days: $(ls "$SCRATCH/data/logs" | tr '\n' ' ')"
DAVY_TOKEN=$(python3 -c 'import secrets; print(secrets.token_urlsafe(32))') "$BIN" serve --data "$SCRATCH/data" --listen "127.0.0.1:$PORT" --public-url "$URL" >"$SCRATCH/server.log" 2>&1 &
PID=$!
for i in {1..100}; do curl -s -o /dev/null "$URL/" && break; sleep 0.1; done
LINK=$("$BIN" user add load@example.com --data "$SCRATCH/data" --public-url "$URL" | grep -o 'http[^ ]*/ui/welcome#[^ ]*')
curl -fsS -c "$SCRATCH/jar.txt" -o /dev/null -H "Origin: $URL" -H 'Content-Type: application/json' \
  --data "{\"secret\":\"${LINK#*#}\",\"password\":\"correct horse battery staple\"}" "$URL/ui/api/welcome"
COOKIE=$(sed 's/^#HttpOnly_//' "$SCRATCH/jar.txt" | awk '!/^#/ && NF>=7 {print $6"="$7}' | head -1)
echo "load before: $(sysctl -n vm.loadavg)"
python3 -I - "$PORT" "$COOKIE" "$N" "$HOURS" <<'PY'
import http.client, json, statistics, sys, time, urllib.parse
port, cookie, n, hours = int(sys.argv[1]), sys.argv[2], int(sys.argv[3]), int(sys.argv[4])
c = http.client.HTTPConnection("127.0.0.1", port)
def get(path):
  c.request("GET", path, headers={"Cookie": cookie})
  r = c.getresponse(); body = r.read()
  assert r.status == 200, (path, r.status, body[:200])
  return json.loads(body)
now = time.time_ns()
hour, day = 3_600_000_000_000, hours * 3_600_000_000_000
def window(span): return f"from={now - span}&to={now}"
q = lambda s: urllib.parse.quote(s)
first = get(f"/ui/api/logs?{window(day)}")
back = first["before"]
for _ in range(20):
  back = get(f"/ui/api/logs?{window(day)}&before={back}")["before"]
newest = first["logs"][0]["timestamp_ns"]
cases = {
  "page, last hour": f"/ui/api/logs?{window(hour)}",
  "page, whole window": f"/ui/api/logs?{window(day)}",
  "page, 21 pages back": f"/ui/api/logs?{window(day)}&before={back}",
  "page, one project": f"/ui/api/logs?{window(day)}&project=web",
  "page, errors": f"/ui/api/logs?{window(day)}&level=error",
  "page, text (timeout)": f"/ui/api/logs?{window(day)}&q=timeout",
  "page, attribute (status 500 POST)": f"/ui/api/logs?{window(day)}&q=" + q("attributes.res.statusCode:500 AND attributes.req.method:post"),
  "page, rare text (no hit)": f"/ui/api/logs?{window(day)}&q=nothingmatches",
  "live, nothing newer": f"/ui/api/logs?{window(day)}&after={newest}",
  "histogram, last hour": f"/ui/api/logs/histogram?{window(hour)}",
  "histogram, whole window": f"/ui/api/logs/histogram?{window(day)}",
  "histogram, errors": f"/ui/api/logs/histogram?{window(day)}&level=error",
  "histogram, text (timeout)": f"/ui/api/logs/histogram?{window(day)}&q=timeout",
  "histogram, one project": f"/ui/api/logs/histogram?{window(day)}&project=web",
}
worst = 0
for name, path in cases.items():
  body = get(path)
  rows = len(body["logs"]) if "logs" in body else sum(body["counts"])
  ts = []
  for _ in range(n):
    t = time.perf_counter(); get(path); ts.append((time.perf_counter() - t) * 1000)
  ts.sort(); p95 = ts[int(len(ts) * 0.95) - 1]; worst = max(worst, p95)
  print(f"{name:36s} rows {rows:9d}  p50 {statistics.median(ts):7.1f} ms  p95 {p95:7.1f} ms  max {ts[-1]:7.1f} ms", flush=True)
print(f"worst p95 {worst:.1f} ms against a budget of 100 ms")
PY
echo "load after: $(sysctl -n vm.loadavg)"
