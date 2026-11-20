#!/bin/bash
set -e
export BENCH_DIR=${BENCH_DIR:-$(cd "$(dirname "$0")/../.." && pwd)/.scratch/bench-data}
B=./target/release/obs-bench
uptime
for i in 1 2 3 4 5; do
  for v in sqlite-fts-word sqlite-fts-word-none tantivy1 tantivy4 sqlite; do
    $B ingest $v 2000000
  done
done
for i in 1 2; do $B ingest sqlite-fts-tri 2000000; done
uptime
for v in sqlite-fts-word tantivy1; do
  $B ingest $v 10000000
  $B query $v
done
$B ingest sqlite-fts-word-none 2000000
$B query sqlite-fts-word-none
uptime
