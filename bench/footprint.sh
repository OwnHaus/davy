#!/bin/bash
# Usage: bench/footprint.sh [quick]
# Measures what Davy needs in disk, memory and CPU for public log corpora,
# the synthetic pino generator and error events, and writes the coefficients
# (schema `davy.footprint`) for a disk and RAM forecast. A full run replaces
# bench/footprint.json, and only when every run in it is complete. `quick`
# runs only the 2k-line samples and small error runs, to test the script,
# and writes its report to the work directory.
#
# macOS only: it needs taskpolicy, /usr/bin/time -l, footprint, sysctl and
# shasum, and a python3 whose SQLite has `dbstat`.
#
# Environment:
#   FOOTPRINT_WORK    downloads, corpora and runs; default .scratch/footprint
#                     in the main checkout, shared by every worktree. Each run
#                     works in its own runs-XXXXXX directory below it.
#   FOOTPRINT_OUT     the report, an absolute path; default as described above.
#   FOOTPRINT_SHARED  1 if other work ran on the machine at the same time,
#                     0 if not. Recorded in the report.
#   FOOTPRINT_LOW_PRIORITY
#                     1 lets a full run start at background or lowered
#                     priority. The report records the priority either way.
#
# Every log corpus goes through `davy serve` over the Logtail route, so the
# default scrubbing, parsing and the writer's commits and merges all apply.
# Downloads are untrusted data. Each is checked against a SHA-256 below,
# read only as text, and an archive is unpacked to stdout, never to the
# paths it names. A download or a corpus is kept only after its SHA-256
# matched, so a broken or partial one is never reused.
#
# Run it at normal priority. The one full run under `taskpolicy -b` took 3
# to 6x the CPU per row of the run before it, and as the log writer commits
# every second, how many segments a run leaves depends on how fast it
# ingests. So a full run refuses to start at a lower priority unless
# FOOTPRINT_LOW_PRIORITY=1, see docs/storage-benchmark.md.
set -euo pipefail
fail() { echo "footprint: $*" >&2; exit 1; }
[ "$(uname)" = Darwin ] || fail "macOS only, see the header"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
QUICK="${1:-}"
case "$QUICK" in '' | quick) ;; *) fail "usage: bench/footprint.sh [quick]" ;; esac
if [ -n "${FOOTPRINT_WORK:-}" ]; then
  WORK="$FOOTPRINT_WORK"
else
  COMMON="$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null || true)"
  MAIN="${COMMON%/.git}"
  if [ -z "$COMMON" ] || [ "$MAIN" = "$COMMON" ] || [ ! -d "$MAIN/.git" ]; then
    fail "not in a git checkout with a .git directory, set FOOTPRINT_WORK"
  fi
  WORK="$MAIN/.scratch/footprint"
fi
case "$WORK" in /?*) ;; *) fail "FOOTPRINT_WORK must be an absolute path" ;; esac
if [ "$QUICK" = quick ]; then
  OUT="${FOOTPRINT_OUT:-$WORK/footprint-quick.json}"
else
  OUT="${FOOTPRINT_OUT:-$ROOT/bench/footprint.json}"
fi
case "$OUT" in /?*) ;; *) fail "FOOTPRINT_OUT must be an absolute path" ;; esac
# `background` under `taskpolicy -b` (PRIO_DARWIN_BG, which children
# inherit), `lowered` under nice or throttled disk I/O, else `normal`. A QoS
# clamp (`taskpolicy -c`) is not detected.
PRIORITY=$(python3 -I -c '
import ctypes, os
background = os.getpriority(4, 0)  # PRIO_DARWIN_PROCESS
disk = ctypes.CDLL(None).getiopolicy_np(0, 0)  # IOPOL_TYPE_DISK, IOPOL_SCOPE_PROCESS
lowered = os.getpriority(os.PRIO_PROCESS, 0) > 0 or disk in (3, 4)  # THROTTLE, UTILITY
print("background" if background else "lowered" if lowered else "normal")')
if [ "$PRIORITY" != normal ]; then
  if [ -z "$QUICK" ] && [ "${FOOTPRINT_LOW_PRIORITY:-}" != 1 ]; then
    fail "this shell runs at $PRIORITY priority and a full run is meant for normal priority; set FOOTPRINT_LOW_PRIORITY=1 to record a run at $PRIORITY priority"
  fi
  echo "footprint: WARNING: running at $PRIORITY priority, the report records it" >&2
fi
LOGHUB=https://raw.githubusercontent.com/logpai/loghub/dd61d0952749ee7963bde24220d1be5ede023033
ZENODO=https://zenodo.org/records/8196385/files
NGINX='https://raw.githubusercontent.com/elastic/examples/79848a10658bd8a272ea24dab36f75cfd92ed0cf/Common%20Data%20Formats/nginx_logs/nginx_logs'
ENVELOPE="$ROOT/e2e/fixtures/sentry-node-10/4.body"

# name|shape|url|sha256 of the download|archive member|first lines only, 0
# for all|sha256 of the corpus
SMALL=(
  "hdfs-2k|line|$LOGHUB/HDFS/HDFS_2k.log|7c967000980c086ed55fa6544ba4f05fe66d44622795e890c68caf8bbb635035||0|7c967000980c086ed55fa6544ba4f05fe66d44622795e890c68caf8bbb635035"
  "apache-2k|line|$LOGHUB/Apache/Apache_2k.log|c7efa3eb686e3a96bd2f8f4457b2a7887e9cf2f3649327f1b4e87af841363ce8||0|c7efa3eb686e3a96bd2f8f4457b2a7887e9cf2f3649327f1b4e87af841363ce8"
  "openssh-2k|line|$LOGHUB/OpenSSH/OpenSSH_2k.log|1e4912727fa88245113d41b16a0cd25ceadba7f931e1c406542885b91254264f||0|1e4912727fa88245113d41b16a0cd25ceadba7f931e1c406542885b91254264f"
  "linux-2k|line|$LOGHUB/Linux/Linux_2k.log|b3e20bc1afe732ab1bf3ed1de4bf9c809e4194e02f7dea911d918e5342e8e173||0|b3e20bc1afe732ab1bf3ed1de4bf9c809e4194e02f7dea911d918e5342e8e173"
  "spark-2k|line|$LOGHUB/Spark/Spark_2k.log|2e8b9a37fc5c238253e0b8e18a8bd5e489671def91767ae1192d28c8e1f95901||0|2e8b9a37fc5c238253e0b8e18a8bd5e489671def91767ae1192d28c8e1f95901"
)
LARGE=(
  "apache|line|$ZENODO/Apache.tar.gz|f2cb5014453c8a0f71596838e1639a2131ff6d360839ad280409f621a6cac452|Apache.log|0|23e616a1ed529bab9b20b9bc345d37035061eee971d246102bcc4937e51a039b"
  "linux|line|$ZENODO/Linux.tar.gz|7e1f820d8d45ae086032e515d9ce8d079a102a9b37dcff5b41a8e60b1f857820|Linux.log|0|b541078328bf5402c254e7e8971a95963c464858031889ff6e15b6d1a857d490"
  "openssh|line|$ZENODO/SSH.tar.gz|296610a35773f58b7c295e5407fc5ccdf48beb3c544fc14a6309addf904a2712|SSH.log|0|89eee9e36c182722f9fadc6e5617a4818ffc66109e7e222b8f9e8b78fc3aa2f3"
  "spark-1m|line|$ZENODO/Spark.tar.gz|f11e5df5a98ce25d0f80adb7631b022ae6f1193f510079387447ef4e0fdccef7|*|1000000|ec36a80261f128fc9abc6619c43eb83403f1323b3a8060244dc50edd49198bb9"
  "hdfs-2m|line|$ZENODO/HDFS_v1.zip|04f919f2185821f23f045dca611a7586429bdabc601bd7b43f30005f8e289b01|HDFS.log|2000000|65fdb863a03ea681cf512c6f13081e1e141b775650b9c7d569eca4eae53fc242"
  "nginx|line|$NGINX|526832433ab552466dc8623390fd92dc052b4f301b2eec94836a5c42a46937df||0|526832433ab552466dc8623390fd92dc052b4f301b2eec94836a5c42a46937df"
  "nginx-parsed|nginx|$NGINX|526832433ab552466dc8623390fd92dc052b4f301b2eec94836a5c42a46937df||0|526832433ab552466dc8623390fd92dc052b4f301b2eec94836a5c42a46937df"
  "synthetic-2m|json|synth|||2000000|04e7685e1b1648bff19002b26274cf1f4df849f3a642ab210142a88483c0f9d4"
)
# The corpora that also run spread over 90 days, for search memory,
# retention and the cost of a day.
SPREAD=(hdfs-2m synthetic-2m)
# The corpora full of IP addresses, which also run with IP scrubbing off:
# the default policy replaces each address with `[Filtered: ip]`.
KEEP_IP=(nginx openssh)

taskpolicy -b cargo build -q --release -j 4 --bin davy
# A separate build, so the example's dev-dependencies cannot add features
# to the measured binary.
taskpolicy -b cargo build -q --release -j 4 --example footprint
COMMIT="$(git describe --always --dirty 2>/dev/null || echo unknown)"
mkdir -p "$WORK/corpora/dl"
# Part files of a run that was killed by SIGKILL. A live run's are younger.
find "$WORK/corpora" -name '*.part.*' -mtime +0 -delete
RUNS="$(mktemp -d "$WORK/runs-XXXXXX")"
# The runs use copies, so a build in this worktree during the run cannot
# swap the binary the report names.
mkdir "$RUNS/bin"
cp "$ROOT/target/release/davy" "$ROOT/target/release/examples/footprint" "$RUNS/bin/"
BIN="$RUNS/bin/davy"
EX="$RUNS/bin/footprint"
TOKEN=$(python3 -I -c 'import secrets; print(secrets.token_urlsafe(32))')
TPID=''
SPID=''
SAMPLER=''
MERGER=''
CHILD=''

# Sends SIGTERM to process $1 and waits up to $2 seconds for it to exit,
# then sends SIGKILL and returns 1.
halt() {
  local i
  kill -TERM "$1" 2>/dev/null || return 0
  for ((i = 0; i < $2 * 10; i++)); do
    kill -0 "$1" 2>/dev/null || return 0
    sleep 0.1
  done
  kill -KILL "$1" 2>/dev/null || true
  return 1
}

# Stops the server within $1 seconds, by SIGKILL if SIGTERM was not enough,
# and returns its exit status as /usr/bin/time passes it on.
halt_server() {
  local pid=$SPID status=0
  # Cleanup can run before start_server has read the server's PID.
  if [ -z "$pid" ]; then
    for _ in {1..50}; do
      pid=$(pgrep -P "$TPID" || true)
      if [ -n "$pid" ] || ! kill -0 "$TPID" 2>/dev/null; then break; fi
      sleep 0.1
    done
  fi
  if ! halt "${pid:-$TPID}" "$1"; then
    echo "footprint: davy serve did not stop within $1 s, killed it" >&2
  fi
  wait "$TPID" || status=$?
  TPID=''
  SPID=''
  return "$status"
}

cleanup() {
  local status=$?
  trap '' HUP INT TERM
  set +e
  if [ -n "$CHILD" ]; then halt "$CHILD" 10; wait "$CHILD" 2>/dev/null; fi
  if [ -n "$MERGER" ]; then halt "$MERGER" 10; wait "$MERGER" 2>/dev/null; fi
  if [ -n "$TPID" ]; then halt_server 60; fi
  if [ -n "$SAMPLER" ]; then kill "$SAMPLER" 2>/dev/null; wait "$SAMPLER" 2>/dev/null; fi
  rm -f "$WORK/corpora/"*".part.$$" "$WORK/corpora/dl/"*".part.$$"
  if [ "$status" = 0 ]; then
    rm -rf "$RUNS"
  else
    rm -rf "$RUNS"/*/data "${RUNS:?}/bin"
    echo "footprint: failed, the logs of each run are in $RUNS" >&2
  fi
}
trap cleanup EXIT
# A signal ends the script with a failure status, so cleanup keeps the
# logs. Bash runs these only between commands, see `child`.
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

# Runs a command in the background and waits for it, because `wait` is a
# command a signal interrupts, so its trap runs now and cleanup stops the
# command. Standard input is passed on, which `&` would replace.
child() {
  local status=0
  "$@" 0<&0 &
  CHILD=$!
  wait "$CHILD" || status=$?
  CHILD=''
  return "$status"
}

kv() { printf '%s=%s\n' "$1" "$2" >> "$RUN/result.env"; }
bytes() { echo $(( $(du -sk "$1" | cut -f1) * 1024 )); }
sha256() { shasum -a 256 < "$1" | cut -d' ' -f1; }
free_port() {
  python3 -I -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()'
}

# Removes $1 and stops unless its SHA-256 is $2.
verify() {
  local got
  got=$(sha256 "$1")
  if [ "$got" != "$2" ]; then
    rm -f "$1"
    fail "$1 has SHA-256 $got, expected $2"
  fi
}

# Downloads $1 to $3 unless a file with SHA-256 $2 is there already.
fetch() {
  if [ -s "$3" ] && [ "$(sha256 "$3")" = "$2" ]; then return; fi
  local part="$3.part.$$"
  curl --proto '=https' --proto-redir '=https' -fsSL --connect-timeout 20 \
    --max-time 1800 --retry 2 --max-filesize 300000000 -o "$part" "$1?download=1"
  verify "$part" "$2"
  mv "$part" "$3"
}

# Writes member $2 of archive $1 to stdout.
unpack() {
  case "$1" in
    *.tar.gz) if [ "$2" = '*' ]; then tar -xzOf "$1"; else tar -xzOf "$1" "$2"; fi ;;
    *.zip) unzip -p "$1" "$2" ;;
    *) cat "$1" ;;
  esac
}

limit() { if [ "$1" = 0 ]; then cat; else head -n "$1"; fi; }

# Sets CORPUS to the path of corpus $1, fetching and unpacking it first
# unless it is there with the expected SHA-256.
corpus() {
  local name=$1 url=$3 sha=$4 member=$5 lines=$6 want=$7
  CORPUS="$WORK/corpora/$name.log"
  if [ -s "$CORPUS" ] && [ "$(sha256 "$CORPUS")" = "$want" ]; then return; fi
  local part="$CORPUS.part.$$"
  if [ "$url" = synth ]; then
    child "$EX" synth "$lines" > "$part"
  else
    local dl
    dl="$WORK/corpora/dl/$(basename "${url%%\?*}")"
    fetch "$url" "$sha" "$dl"
    # head closes the pipe early and tar reports that as an error, so only
    # a whole member must unpack cleanly. The SHA-256 catches a short prefix.
    if ! unpack "$dl" "$member" | limit "$lines" > "$part"; then
      [ "$lines" != 0 ] || { rm -f "$part"; fail "unpacking $member of $dl failed"; }
    fi
  fi
  verify "$part" "$want"
  mv "$part" "$CORPUS"
}

# Starts `davy serve` on data dir $1 under /usr/bin/time, its log in $RUN/$2.
# It is up once its own PID listens on the port, so neither a proxy nor
# another process that took the port can pass for it.
start_server() {
  PORT=$(free_port)
  DAVY_TOKEN="$TOKEN" /usr/bin/time -l -o "$RUN/$2.time" "$BIN" serve --data "$1" \
    --listen "127.0.0.1:$PORT" --public-url "http://127.0.0.1:$PORT" \
    --log-retention-days 100 > "$RUN/$2.log" 2>&1 &
  TPID=$!
  SPID=''
  local deadline=$((SECONDS + 60))
  until [ -n "$SPID" ] && lsof -a -p "$SPID" -nP -iTCP:"$PORT" -sTCP:LISTEN -t > /dev/null 2>&1; do
    kill -0 "$TPID" 2>/dev/null || fail "davy serve exited at start, see $RUN/$2.log"
    [ "$SECONDS" -lt "$deadline" ] || fail "davy serve did not listen within 60 s, see $RUN/$2.log"
    [ -n "$SPID" ] || SPID=$(pgrep -P "$TPID" || true)
    sleep 0.1
  done
}

# Stops the server, which commits and waits for its merges, and records what
# /usr/bin/time saw under the prefix $1. Peak RSS counts the index files the
# server mapped and read; peak footprint leaves out such clean file pages.
# A slow merge at background priority can take minutes, hence the 600 s.
stop_server() {
  local status=0
  halt_server 600 || status=$?
  [ "$status" = 0 ] || fail "davy serve exited with status $status, see $RUN/$1.log"
  local t="$RUN/$1.time"
  kv "${1}_wall_secs" "$(awk '/ real / {print $1}' "$t")"
  kv "${1}_cpu_secs" "$(awk '/ real / {print $3 + $5}' "$t")"
  kv "${1}_peak_rss_bytes" "$(awk '/maximum resident set size/ {print $1}' "$t")"
  kv "${1}_peak_footprint_bytes" "$(awk '/peak memory footprint/ {print $1}' "$t")"
}

# Samples the size of $1 every 0.2 s while process $2 and this script run,
# the peak into $3.
sample_peak() {
  (
    peak=0
    while kill -0 "$2" 2>/dev/null && kill -0 $$ 2>/dev/null; do
      k=$(du -sk "$1" 2>/dev/null | cut -f1 || true)
      if [ "${k:-0}" -gt "$peak" ]; then peak=$k; fi
      sleep 0.2
    done
    echo $(( peak * 1024 )) > "$3"
  ) &
  SAMPLER=$!
}

new_run() {
  RUN="$RUNS/$1"
  mkdir "$RUN"
}

project() {
  local dsn
  dsn=$("$BIN" project create bench --data "$1" --public-url "http://127.0.0.1:1")
  # The corpus goes in as fast as the server takes it, not at a project's rate.
  "$BIN" project quota bench --data "$1" --spike off --spike-logs off > /dev/null
  KEY="${dsn#*://}"; KEY="${KEY%%@*}"
  PROJECT="${dsn##*/}"
}

# Times searches over the last 7, 30 and 90 days through MCP, and reads the
# server's RSS and footprint after each window. Each window runs a query with
# no hit, which opens every day in it, and the newest 100 rows. Both touch
# little data, and the page cache is warm from the ingest just before.
search_rss() {
  child python3 -I - "$PORT" "$TOKEN" "$SPID" >> "$RUN/result.env" <<'PY'
import http.client, json, re, subprocess, sys, time
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
UNITS = {"B": 1, "KB": 1024, "MB": 1024 ** 2, "GB": 1024 ** 3}
def footprint():
  out = subprocess.run(["footprint", "-p", pid], capture_output=True, text=True).stdout
  n, unit = re.search(r"Footprint: ([\d.]+) (B|KB|MB|GB)", out).groups()
  return round(float(n) * UNITS[unit])
def search(args):
  t = time.perf_counter()
  out = rpc("tools/call", {"name": "search_logs", "arguments": args})
  assert "result" in out and not out["result"].get("isError"), str(out)[:400]
  return (time.perf_counter() - t) * 1000
rpc("initialize", {"protocolVersion": "2025-06-18", "capabilities": {},
                   "clientInfo": {"name": "footprint-bench", "version": "1"}})
rpc("notifications/initialized", {}, id=None)
print(f"search_rss_start_bytes={rss()}")
print(f"search_footprint_start_bytes={footprint()}")
for days in (7, 30, 90):
  miss = search({"since": f"{days}d", "query": "nothingmatchesthis", "limit": 10})
  newest = search({"since": f"{days}d", "limit": 100})
  print(f"search_{days}d_miss_first_ms={miss:.1f}")
  print(f"search_{days}d_newest_ms={newest:.1f}")
  print(f"search_rss_{days}d_bytes={rss()}")
  print(f"search_footprint_{days}d_bytes={footprint()}")
PY
}

retention() {
  local logs="$1/logs"
  kv retention_before_bytes "$(bytes "$logs")"
  kv retention_before_days "$(find "$logs" -maxdepth 1 -name '20*' | wc -l | tr -d ' ')"
  child "$EX" retain "$1" 30 >> "$RUN/result.env"
  kv retention_after_bytes "$(bytes "$logs")"
  kv retention_after_days "$(find "$logs" -maxdepth 1 -name '20*' | wc -l | tr -d ' ')"
}

# Forces a merge of each day into one segment and measures its peak disk.
merge_days() {
  "$EX" merge "$1" >> "$RUN/result.env" &
  MERGER=$!
  sample_peak "$1/logs" "$MERGER" "$RUN/merge-peak"
  wait "$MERGER"
  MERGER=''
  wait "$SAMPLER" || true; SAMPLER=''
  kv merge_peak_disk_bytes "$(cat "$RUN/merge-peak")"
  kv one_segment_bytes "$(bytes "$1/logs")"
}

# run_logs ENTRY DAYS SCRUB: ingests one corpus, then measures it. SCRUB is
# `default` or `keep-ip`.
run_logs() {
  local name shape url sha member lines want
  IFS='|' read -r name shape url sha member lines want <<< "$1"
  local days=$2 scrub=$3 data suffix=''
  corpus "$name" "$shape" "$url" "$sha" "$member" "$lines" "$want"
  if [ "$scrub" = keep-ip ]; then suffix=-keep-ip; fi
  new_run "logs-$name-${days}d$suffix"
  data="$RUN/data"
  project "$data"
  if [ "$scrub" = keep-ip ]; then
    "$BIN" project scrub bench --data "$data" --settings '{"ip": false}' > /dev/null
  fi
  kv kind logs; kv corpus "$name"; kv shape "$shape"; kv days "$days"; kv scrub "$scrub"
  kv source "$url"; kv sha256 "$sha"; kv member "$member"; kv corpus_sha256 "$want"
  start_server "$data" ingest
  kv load_at_start "$(sysctl -n vm.loadavg)"
  echo "$name over $days days, scrub $scrub: load $(sysctl -n vm.loadavg)"
  sample_peak "$data/logs" "$SPID" "$RUN/ingest-peak"
  child "$EX" logs "127.0.0.1:$PORT" "$KEY" "$CORPUS" "$shape" "$days" >> "$RUN/result.env"
  stop_server ingest
  wait "$SAMPLER" || true; SAMPLER=''
  kv ingest_peak_disk_bytes "$(cat "$RUN/ingest-peak")"
  kv disk_bytes "$(bytes "$data/logs")"
  # The pattern counters of each project and day, in the day's directory, so
  # disk_bytes counts them.
  kv patterns_files "$(find "$data/logs" -name 'patterns-*.json' | wc -l | tr -d ' ')"
  kv patterns_bytes "$(find "$data/logs" -name 'patterns-*.json' -exec stat -f %z {} + | awk '{s += $1} END {print s + 0}')"
  kv patterns_disk_bytes "$(find "$data/logs" -name 'patterns-*.json' -exec du -k {} + | awk '{s += $1} END {print s * 1024}')"
  kv sqlite_bytes "$(cat "$data"/davy.sqlite* | wc -c | tr -d ' ')"
  "$EX" inspect "$data" >> "$RUN/result.env"
  if [ "$days" -gt 1 ]; then
    start_server "$data" search
    search_rss
    stop_server search
    retention "$data"
  else
    merge_days "$data"
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
  kv load_at_start "$(sysctl -n vm.loadavg)"
  echo "errors $1 x $2: load $(sysctl -n vm.loadavg)"
  child "$EX" errors "127.0.0.1:$PORT" "$KEY" "$PROJECT" "$ENVELOPE" "$1" "$2" 7 >> "$RUN/result.env"
  stop_server ingest
  child python3 -I - "$data/davy.sqlite" "$data/compacted.sqlite" >> "$RUN/result.env" <<'PY'
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
# The newest event of an issue outside the sample keeps its payload in
# latest_payloads, one row per issue, so a payload is in one table or the other.
print(f"payloads={one('SELECT count(payload) FROM events') + one('SELECT count(*) FROM latest_payloads')}")
print(f"payload_bytes={one('SELECT coalesce(sum(length(payload)), 0) FROM events') + one('SELECT coalesce(sum(length(payload)), 0) FROM latest_payloads')}")
print(f"latest_payloads={one('SELECT count(*) FROM latest_payloads')}")
print(f"freelist_bytes={one('PRAGMA freelist_count') * one('PRAGMA page_size')}")
print(f"hourly_rows={one('SELECT count(*) FROM issue_hourly')}")
print(f"tag_rows={one('SELECT count(*) FROM issue_tags')}")
# Space freed inside the pages of events stays there. A compacted copy shows
# what the same rows take without such holes.
unused, btree = c.execute("SELECT sum(unused), sum(pgsize) FROM dbstat WHERE name='events'").fetchone()
print(f"events_unused_bytes={unused}")
print(f"events_btree_bytes={btree}")
c.execute("VACUUM INTO ?", (sys.argv[2],))
v = sqlite3.connect(sys.argv[2])
print(f"compacted_sqlite_bytes={v.execute('PRAGMA page_count').fetchone()[0] * v.execute('PRAGMA page_size').fetchone()[0]}")
events = v.execute("SELECT sum(pgsize) FROM dbstat WHERE name IN (SELECT name FROM sqlite_master WHERE tbl_name IN ('events', 'latest_payloads'))").fetchone()[0]
print(f"compacted_table_events_bytes={events}")
PY
  rm -rf "$data"
}

# run_entry ENTRY: one corpus over one day, then the variants it is listed for.
run_entry() {
  local name=${1%%|*} s
  run_logs "$1" 1 default
  for s in "${KEEP_IP[@]}"; do
    if [ "$name" = "$s" ]; then run_logs "$1" 1 keep-ip; fi
  done
  for s in "${SPREAD[@]}"; do
    if [ "$name" = "$s" ]; then run_logs "$1" 90 default; fi
  done
}

echo "load at start: $(sysctl -n vm.loadavg)"
for entry in "${SMALL[@]}"; do run_logs "$entry" 1 default; done
if [ "$QUICK" = quick ]; then
  run_errors 100 20
  run_errors 2 1000
else
  for entry in "${LARGE[@]}"; do run_entry "$entry"; done
  run_errors 5000 20
  run_errors 10 10000
fi
echo "load at end: $(sysctl -n vm.loadavg)"
python3 -I "$ROOT/bench/footprint-report.py" "$RUNS" "$OUT" "$COMMIT" "${FOOTPRINT_SHARED:-}" "$PRIORITY"
