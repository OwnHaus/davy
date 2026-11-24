#!/bin/bash
# Usage: bench/footprint.sh [quick]
# Measures what Davy needs in disk, memory and CPU for public log corpora,
# the synthetic pino generator and error events, and writes the coefficients
# to bench/footprint.json (schema `davy.footprint`) for a disk and RAM
# forecast. `quick` runs only the 2k-line samples and small error runs, to
# test the script.
#
# Every log corpus goes through `davy serve` over the Logtail route, so the
# default scrubbing, parsing and the writer's commits and merges all apply.
# Downloads are untrusted data. Each is checked against a SHA-256 below,
# read only as text, and an archive is unpacked to stdout, never to the
# paths it names. They live in the main checkout's .scratch/footprint, not
# in the repo.
#
# Run it at normal priority: under `taskpolicy -b` times are 3 to 4x slower.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
MAIN="$(cd "$(git rev-parse --git-common-dir)/.." && pwd)"
WORK="${FOOTPRINT_WORK:-$MAIN/.scratch/footprint}"
OUT="${FOOTPRINT_OUT:-$ROOT/bench/footprint.json}"
QUICK="${1:-}"
LOGHUB=https://raw.githubusercontent.com/logpai/loghub/master
ZENODO=https://zenodo.org/records/8196385/files
NGINX='https://raw.githubusercontent.com/elastic/examples/master/Common%20Data%20Formats/nginx_logs/nginx_logs'
ENVELOPE="$ROOT/e2e/fixtures/sentry-node-10/4.body"

# name|shape|url|sha256 of the download|archive member|lines, 0 for all
SMALL=(
  "hdfs-2k|line|$LOGHUB/HDFS/HDFS_2k.log|7c967000980c086ed55fa6544ba4f05fe66d44622795e890c68caf8bbb635035||0"
  "apache-2k|line|$LOGHUB/Apache/Apache_2k.log|c7efa3eb686e3a96bd2f8f4457b2a7887e9cf2f3649327f1b4e87af841363ce8||0"
  "openssh-2k|line|$LOGHUB/OpenSSH/OpenSSH_2k.log|1e4912727fa88245113d41b16a0cd25ceadba7f931e1c406542885b91254264f||0"
  "linux-2k|line|$LOGHUB/Linux/Linux_2k.log|b3e20bc1afe732ab1bf3ed1de4bf9c809e4194e02f7dea911d918e5342e8e173||0"
  "spark-2k|line|$LOGHUB/Spark/Spark_2k.log|2e8b9a37fc5c238253e0b8e18a8bd5e489671def91767ae1192d28c8e1f95901||0"
)
LARGE=(
  "apache|line|$ZENODO/Apache.tar.gz|f2cb5014453c8a0f71596838e1639a2131ff6d360839ad280409f621a6cac452|Apache.log|0"
  "linux|line|$ZENODO/Linux.tar.gz|7e1f820d8d45ae086032e515d9ce8d079a102a9b37dcff5b41a8e60b1f857820|Linux.log|0"
  "openssh|line|$ZENODO/SSH.tar.gz|296610a35773f58b7c295e5407fc5ccdf48beb3c544fc14a6309addf904a2712|SSH.log|0"
  "spark-1m|line|$ZENODO/Spark.tar.gz|f11e5df5a98ce25d0f80adb7631b022ae6f1193f510079387447ef4e0fdccef7|*|1000000"
  "hdfs-2m|line|$ZENODO/HDFS_v1.zip|04f919f2185821f23f045dca611a7586429bdabc601bd7b43f30005f8e289b01|HDFS.log|2000000"
  "nginx|line|$NGINX|526832433ab552466dc8623390fd92dc052b4f301b2eec94836a5c42a46937df||0"
  "nginx-parsed|nginx|$NGINX|526832433ab552466dc8623390fd92dc052b4f301b2eec94836a5c42a46937df||0"
  "synthetic-2m|json|synth|||2000000"
)
# The corpora that also run spread over 90 days, for search memory,
# retention and the cost of a day.
SPREAD=(hdfs-2m synthetic-2m)

taskpolicy -b cargo build --release -j 4 --bin davy --example footprint 2>&1 | tail -1
BIN="$ROOT/target/release/davy"
EX="$ROOT/target/release/examples/footprint"
rm -rf "$WORK/runs"
mkdir -p "$WORK/corpora/dl" "$WORK/runs"
TOKEN=$(python3 -I -c 'import secrets; print(secrets.token_urlsafe(32))')
TPID=''
SPID=''
SAMPLER=''
cleanup() {
  if [ -n "$SPID" ]; then kill -TERM "$SPID" 2>/dev/null || true; fi
  if [ -n "$TPID" ]; then wait "$TPID" 2>/dev/null || true; fi
  if [ -n "$SAMPLER" ]; then kill "$SAMPLER" 2>/dev/null || true; fi
}
trap cleanup EXIT

kv() { printf '%s=%s\n' "$1" "$2" >> "$RUN/result.env"; }
bytes() { echo $(( $(du -sk "$1" | cut -f1) * 1024 )); }
free_port() {
  python3 -I -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()'
}

limit() { if [ "$1" = 0 ]; then cat; else head -n "$1"; fi; }

# Prints the path of corpus $1, fetching and checking it first.
corpus() {
  local name=$1 url=$3 sha=$4 member=$5 lines=$6
  local file="$WORK/corpora/$name.log"
  if [ "$url" = synth ]; then
    [ -s "$file" ] || "$EX" synth "$lines" > "$file"
    echo "$file"
    return
  fi
  local dl
  dl="$WORK/corpora/dl/$(basename "${url%%\?*}")"
  if [ ! -s "$dl" ]; then
    curl -fsSL --max-filesize 300000000 -o "$dl.part" "$url?download=1"
    mv "$dl.part" "$dl"
  fi
  local got
  got=$(shasum -a 256 "$dl" | cut -d' ' -f1)
  if [ "$got" != "$sha" ]; then
    echo "footprint: $dl has SHA-256 $got, expected $sha" >&2
    exit 1
  fi
  if [ ! -s "$file" ]; then
    # head closes the pipe early, so tar and unzip may die of SIGPIPE.
    case "$dl" in
      *.tar.gz) if [ "$member" = '*' ]; then { tar -xzOf "$dl" || true; }; else { tar -xzOf "$dl" "$member" || true; }; fi ;;
      *.zip) { unzip -p "$dl" "$member" || true; } ;;
      *) cat "$dl" ;;
    esac | limit "$lines" > "$file.part"
    local n
    n=$(wc -l < "$file.part" | tr -d ' ')
    if [ "$n" = 0 ] || { [ "$lines" != 0 ] && [ "$n" != "$lines" ]; }; then
      echo "footprint: $name has $n lines, expected $lines (0 for any but none)" >&2
      exit 1
    fi
    mv "$file.part" "$file"
  fi
  echo "$file"
}

# Starts `davy serve` on data dir $1 under /usr/bin/time, its log in $RUN/$2.
start_server() {
  PORT=$(free_port)
  DAVY_TOKEN="$TOKEN" /usr/bin/time -l "$BIN" serve --data "$1" --listen "127.0.0.1:$PORT" \
    --public-url "http://127.0.0.1:$PORT" --log-retention-days 100 > "$RUN/$2.log" 2>&1 &
  TPID=$!
  for _ in {1..100}; do curl -s -o /dev/null "http://127.0.0.1:$PORT/" && break; sleep 0.1; done
  SPID=$(pgrep -P "$TPID")
}

# Stops the server, which commits and waits for its merges, and records what
# /usr/bin/time saw under the prefix $1.
stop_server() {
  kill -TERM "$SPID"
  wait "$TPID" || true
  TPID=''
  SPID=''
  local log="$RUN/$1.log"
  kv "${1}_wall_secs" "$(awk '/ real / {print $1}' "$log")"
  kv "${1}_cpu_secs" "$(awk '/ real / {print $3 + $5}' "$log")"
  kv "${1}_peak_rss_bytes" "$(awk '/maximum resident set size/ {print $1}' "$log")"
}

# Samples the size of $1 every 0.2 s while process $2 runs, the peak into $3.
sample_peak() {
  (
    peak=0
    while kill -0 "$2" 2>/dev/null; do
      k=$(du -sk "$1" 2>/dev/null | cut -f1 || true)
      if [ "${k:-0}" -gt "$peak" ]; then peak=$k; fi
      sleep 0.2
    done
    echo $(( peak * 1024 )) > "$3"
  ) &
  SAMPLER=$!
}

new_run() {
  RUN="$WORK/runs/$1"
  rm -rf "$RUN"
  mkdir -p "$RUN"
}

project() {
  local dsn
  dsn=$("$BIN" project create bench --data "$1" --public-url "http://127.0.0.1:1")
  KEY="${dsn#*://}"; KEY="${KEY%%@*}"
  PROJECT="${dsn##*/}"
}

# Times searches over the last 7, 30 and 90 days through MCP, and reads the
# server's RSS after each window. Each window runs a query with no hit, which
# opens every day in it, and the newest 100 rows.
search_rss() {
  python3 -I - "$PORT" "$TOKEN" "$SPID" >> "$RUN/result.env" <<'PY'
import http.client, json, subprocess, sys, time
port, token, pid = int(sys.argv[1]), sys.argv[2], sys.argv[3]
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
def rss():
  return int(subprocess.run(["ps", "-o", "rss=", "-p", pid], capture_output=True, text=True).stdout) * 1024
def search(args):
  t = time.perf_counter()
  out = rpc("tools/call", {"name": "search_logs", "arguments": args})
  assert "result" in out and not out["result"].get("isError"), str(out)[:400]
  return (time.perf_counter() - t) * 1000
rpc("initialize", {"protocolVersion": "2025-06-18", "capabilities": {},
                   "clientInfo": {"name": "footprint-bench", "version": "1"}})
rpc("notifications/initialized", {}, id=None)
print(f"search_rss_start_bytes={rss()}")
for days in (7, 30, 90):
  miss = search({"since": f"{days}d", "query": "nothingmatchesthis", "limit": 10})
  newest = search({"since": f"{days}d", "limit": 100})
  print(f"search_{days}d_miss_first_ms={miss:.1f}")
  print(f"search_{days}d_newest_ms={newest:.1f}")
  print(f"search_rss_{days}d_bytes={rss()}")
PY
}

retention() {
  local logs="$1/logs"
  kv retention_before_bytes "$(bytes "$logs")"
  kv retention_before_days "$(find "$logs" -maxdepth 1 -name '20*' | wc -l | tr -d ' ')"
  "$EX" retain "$1" 30 >> "$RUN/result.env"
  kv retention_after_bytes "$(bytes "$logs")"
  kv retention_after_days "$(find "$logs" -maxdepth 1 -name '20*' | wc -l | tr -d ' ')"
}

# run_logs ENTRY DAYS: ingests one corpus, then measures it.
run_logs() {
  IFS='|' read -r name shape url sha member lines <<< "$1"
  local days=$2 file data
  file=$(corpus "$name" "$shape" "$url" "$sha" "$member" "$lines")
  new_run "logs-$name-${days}d"
  data="$RUN/data"
  project "$data"
  kv kind logs; kv corpus "$name"; kv shape "$shape"; kv days "$days"
  kv source "$url"; kv sha256 "$sha"; kv member "$member"
  start_server "$data" ingest
  echo "$name over $days days: load $(sysctl -n vm.loadavg)"
  sample_peak "$data/logs" "$SPID" "$RUN/ingest-peak"
  "$EX" logs "127.0.0.1:$PORT" "$KEY" "$file" "$shape" "$days" >> "$RUN/result.env"
  stop_server ingest
  wait "$SAMPLER" || true; SAMPLER=''
  kv ingest_peak_disk_bytes "$(cat "$RUN/ingest-peak")"
  kv disk_bytes "$(bytes "$data/logs")"
  kv sqlite_bytes "$(cat "$data"/davy.sqlite* | wc -c | tr -d ' ')"
  "$EX" inspect "$data" >> "$RUN/result.env"
  if [ "$days" -gt 1 ]; then
    start_server "$data" search
    search_rss
    stop_server search
    retention "$data"
  else
    "$EX" merge "$data" >> "$RUN/result.env" &
    local merger=$!
    sample_peak "$data/logs" "$merger" "$RUN/merge-peak"
    wait "$merger"
    wait "$SAMPLER" || true; SAMPLER=''
    kv merge_peak_disk_bytes "$(cat "$RUN/merge-peak")"
    kv one_segment_bytes "$(bytes "$data/logs")"
  fi
  rm -rf "$data"
}

# run_errors ISSUES PER_ISSUE: ingests that many copies of a captured
# Node.js error event, then reads what each table and index takes.
run_errors() {
  new_run "errors-$1x$2"
  local data="$RUN/data"
  project "$data"
  kv kind errors; kv issues "$1"; kv per_issue "$2"; kv days 7
  kv envelope "${ENVELOPE#"$ROOT"/}"
  start_server "$data" ingest
  echo "errors $1 x $2: load $(sysctl -n vm.loadavg)"
  "$EX" errors "127.0.0.1:$PORT" "$KEY" "$PROJECT" "$ENVELOPE" "$1" "$2" 7 >> "$RUN/result.env"
  stop_server ingest
  python3 -I - "$data/davy.sqlite" "$data/compacted.sqlite" >> "$RUN/result.env" <<'PY'
import sqlite3, sys
c = sqlite3.connect(sys.argv[1])
one = lambda q: c.execute(q).fetchone()[0]
print(f"sqlite_bytes={one('PRAGMA page_count') * one('PRAGMA page_size')}")
print(f"page_size={one('PRAGMA page_size')}")
# Each index counts towards the table it indexes.
owner = dict(c.execute("SELECT name, tbl_name FROM sqlite_master"))
tables = {}
for name, size in c.execute("SELECT name, pgsize FROM dbstat WHERE aggregate=TRUE"):
  table = owner.get(name, name)
  tables[table] = tables.get(table, 0) + size
for table, size in tables.items():
  print(f"table_{table}_bytes={size}")
print(f"stored_events={one('SELECT count(*) FROM events')}")
print(f"stored_issues={one('SELECT count(*) FROM issues')}")
print(f"payloads={one('SELECT count(payload) FROM events')}")
print(f"payload_bytes={one('SELECT coalesce(sum(length(payload)), 0) FROM events')}")
print(f"hourly_rows={one('SELECT count(*) FROM issue_hourly')}")
print(f"tag_rows={one('SELECT count(*) FROM issue_tags')}")
# An event's payload is dropped by an UPDATE once a newer event of its
# issue arrives, which leaves the space in its page. A compacted copy shows
# what the same rows take without those holes.
unused = c.execute("SELECT sum(unused) FROM dbstat WHERE name='events'").fetchone()[0]
print(f"events_unused_bytes={unused}")
c.execute("VACUUM INTO ?", (sys.argv[2],))
v = sqlite3.connect(sys.argv[2])
print(f"compacted_sqlite_bytes={v.execute('PRAGMA page_count').fetchone()[0] * v.execute('PRAGMA page_size').fetchone()[0]}")
events = v.execute("SELECT sum(pgsize) FROM dbstat WHERE name IN (SELECT name FROM sqlite_master WHERE tbl_name='events')").fetchone()[0]
print(f"compacted_table_events_bytes={events}")
PY
  rm -rf "$data"
}

echo "load at start: $(sysctl -n vm.loadavg)"
for entry in "${SMALL[@]}"; do run_logs "$entry" 1; done
if [ "$QUICK" = quick ]; then
  run_errors 100 20
  run_errors 2 1000
else
  for entry in "${LARGE[@]}"; do
    run_logs "$entry" 1
    for s in "${SPREAD[@]}"; do
      if [ "${entry%%|*}" = "$s" ]; then run_logs "$entry" 90; fi
    done
  done
  run_errors 5000 20
  run_errors 10 10000
fi
echo "load at end: $(sysctl -n vm.loadavg)"
python3 -I "$ROOT/bench/footprint-report.py" "$WORK/runs" "$OUT" "$(git rev-parse --short HEAD)"
