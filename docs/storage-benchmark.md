# Log storage benchmark

Decision: logs go into **Tantivy**, one index per day, with the documents stored zstd-compressed inside Tantivy. SQLite stays for metadata (projects, issues, events, sources). SQLite FTS5 is not used for logs.

Measured on 2026-11-20 with `bench/storage` (`run.sh`, raw output in `bench/storage/results-2026-11-20.txt`).

## What was measured

- Synthetic pino-shaped logs, seed 42, 394 bytes per row on average (message plus attributes). The mix is 70% HTTP request logs, 20% info, 7% warn and 3% errors with a stack trace. The generator alone produces 1.32M rows/s on its own thread, so it does not limit any variant.
- Ingest: 2M rows per run, 5 rounds with the variants interleaved, median and range reported. Trigram ran 2 rounds. A scale run then ingested 10M rows into SQLite FTS and Tantivy and ran the queries 5 times each.
- SQLite: page size 8192, WAL, `synchronous=NORMAL`, 256 MB cache, 5000 rows per transaction, FTS5 as external content over the `logs` table.
- Tantivy 0.27: one writer thread (`tantivy1`) or four (`tantivy4`), 256 MB writer budget, a commit every 100k rows, waiting for merges at the end.
- Machine: the shared dev Mac, load average 12 to 21 throughout, with Codex builds running at the same time.

## Results

| Variant | Ingest rows/s, median (range) | Disk / raw | Notes |
|---|---|---|---|
| SQLite, no full-text index | 704k (612k to 806k) | 1.08× | Finding a trace id is a full scan (309 ms at 300k rows, linear) |
| SQLite FTS5 `unicode61` | 67.5k (40.3k to 68.4k) | 1.80× | 52.5k/s at 10M rows, dropping to 30k/s in some million-row stretches |
| SQLite FTS5 `unicode61`, `detail=none` | 100.4k (58.3k to 103.0k) | 1.36× | Cannot scope a query to a column (`message:timeout`) or match a phrase |
| SQLite FTS5 trigram | 10.6k (10.5k to 10.7k) | 5.46× | Under the 20k/s bar and 5.5× the raw size |
| Tantivy, 1 writer thread | 65.9k (39.0k to 69.2k) | 0.69× | 58.0k/s and 0.67× at 10M rows |
| Tantivy, 4 writer threads | 59.0k (45.4k to 70.0k) | 0.68× | 135k/s while indexing, but the final merge dominates the total |
| zstd segments, no index | 951k (1 run, 300k rows) | 0.18× | Smoke run only. A search is a full scan |

Queries at 10M rows (4.2 GB raw), median of 5 runs, in ms:

| Query | SQLite FTS5 | Tantivy |
|---|---|---|
| Trace id (1 hit) | 0.0 | 0.0 |
| `message:timeout`, latest 100 | 0.1 | 0.5 |
| `message:timeout`, count (42,731 hits) | 1.2 | 0.0 |
| Errors in the last hour, latest 100 | 0.5 | 1.3 |
| Count by level in the last hour | 98.0 | 4.2 |

## Why the numbers look like this

- **SQLite FTS5 is bound by one CPU core.** CPU time equals wall time (170 s CPU over 190 s for 10M rows). Tokenizing and the FTS b-tree inserts run on the one writer thread.
- **Tantivy uses about 1.7 cores with one writer thread.** 340 s CPU over 172 s for 10M rows. Segment merges run on background threads. With four writer threads, indexing alone runs at 135k/s, and the wait for the final merge sets the total.
- **The disk gap comes from storage layout, and it is stable.** Disk sizes were identical in every round. Tantivy compresses stored documents with zstd in blocks. SQLite keeps the row uncompressed, and FTS5 adds a position list for every token.
- **Throughput does not decide this.** 300 GB of raw logs a month is about 290 rows/s on average (estimate). Even a 10× peak is under 3k/s, and every variant except trigram has more than 10× headroom on top of that.

## Checks

- Hit counts match across the engines. `timeout` gives 42,731 hits at 10M rows in both SQLite FTS5 and Tantivy. Every engine finds the trace id once.
- The benchmark had a bug that hid errors. A failing query row was counted as a hit, so `detail=none` reported 1 hit for a query it cannot run. `count` now unwraps each row, and the rerun panics with `fts5: column queries are not supported (detail=none)`.
- Noise is high (load 12 to 21, first rounds slower for all variants). The throughput ranges overlap, so the throughput order of FTS5 against Tantivy is not settled. The decision does not rest on it. It rests on disk size and aggregation speed, which do not move with load.

## Decision and tradeoffs

Tantivy, because for a customer with 300 GB of raw logs a month:

- It needs about 200 GB of disk instead of 540 GB with SQLite FTS5 (0.67× against 1.80×, measured ratios applied to the estimate).
- Counts and histograms, which every log view draws, are 23× faster (4.2 ms against 98 ms at 10M rows).
- Retention can delete one directory per day. With FTS5 it means deleting rows and rewriting the index (inferred, not measured).

What it costs:

- Peak RSS reached 2.3 GB at 10M rows in one index, against 0.4 GB for SQLite. Mapped segment files are probably most of that (guess). One index per day bounds it, and RSS gets measured again when logs ship.
- A second storage engine to learn and upgrade, and a larger binary and a longer build (not measured).
- A row is not visible until the next commit. The plan for milestone 3 is a commit every second or every 100k rows, whichever comes first, so a tail view lags by up to a second.

Rejected:

- FTS5 trigram. 10.6k rows/s and 5.5× the raw size.
- FTS5 with `detail=none`. Without column scoping and phrases, there is no `service:api` or exact-message search.
- Unindexed zstd segments. Smallest on disk (0.18×), but every search scans everything. Worth revisiting later as cold storage behind the Tantivy index.

## Logs ingest (milestone 3)

Measured on 2026-11-20 with `bench/load-logs.sh 1000000 1000 4`. It starts `davy serve` and sends 1M synthetic pino-shaped rows (`examples/load_logs.rs`, 260 bytes of NDJSON per row) over the Logtail route as gzip msgpack batches of 1000 from 4 connections. A `429` is retried after 50 ms. It then waits until `davy logs` sees every row, and measures disk with `du`. The shared dev Mac had a load average of 6 to 11 throughout.

| Run | Accepted in | Committed after the last 202 | Sustained | `429`s | Max RSS | Disk / raw |
|---|---|---|---|---|---|---|
| queue of 256 batches | 4.64 s | 4.83 s | 106k rows/s | 268 | 828 MB | 0.48× |
| queue of 50k rows, run 1 | 6.19 s | 2.63 s | 113k rows/s | 378 | 426 MB | 0.48× |
| queue of 50k rows, run 2 | 6.45 s | 3.46 s | 101k rows/s | 391 | 381 MB | 0.48× |
| 50k rows, under `taskpolicy -b` | 58.8 s | 7.02 s | 15k rows/s | 917 | 654 MB | 0.55× |

Sustained means 1M rows over accept time plus commit time. Disk is 125 MB for 260 MB of raw NDJSON.

- **The limit is the indexing thread, as in the storage benchmark.** The server used 15 to 16 s of CPU for 1M rows, 15 µs per row, over about 9.5 s of wall time, so about 1.6 cores (measured). One indexing thread plus the merge threads fits that, the same 1.7 cores Tantivy used above (inferred). The rows here are 260 bytes against 394 there, which fits 100k rows/s here against 58k to 66k there (inferred).
- **A 100k-row run does not measure indexing.** Three runs of 100k rows reported 110k to 259k rows/s accepted, because the old 256-batch queue held all 100 batches and the server indexed them after the client was done. Only the 1M run reaches a steady state.
- **The row-bounded queue halved peak memory at the same throughput.** 256 batches of 1000 rows let up to 256k parsed records wait in memory. 50k rows cut max RSS from 828 MB to 381 to 426 MB. What remains is the 64 MB writer heap, up to 50k queued records, and mapped segment files (inferred, not broken down).
- **Background QoS is 7× slower.** Under `taskpolicy -b` macOS keeps the process on the efficiency cores, so that run measured the scheduler, not Davy. Run the benchmark at normal priority.
- **The JSON attributes field stays.** 0.48× the raw size at 1M rows, against 0.67× to 0.69× for the flat text field in the storage benchmark. The rows differ (260 against 394 bytes), so this is not a like-for-like comparison, but both are under the 1.0× bar.
- Queries over the 1M rows took 87 to 139 ms each through `davy logs` including process start, measured during the `taskpolicy -b` run: the latest 100 rows, `timeout`, and `attributes.req.method:post AND attributes.res.statusCode:500`.

**A bug found by this benchmark.** In the first 100k run the server accepted every batch, but nothing was ever committed, and the process ignored SIGTERM. A stack sample showed the log writer blocked in `IndexWriter::add_document`, with no Tantivy indexing thread left. When a Tantivy indexing worker fails, `prepare_commit` returns the error before it starts a new worker, and `add_document` then blocks on the full channel. The writer now drops a failing `IndexWriter` and opens a new one, and `store_recovers_when_a_day_index_breaks` reproduces the failure by deleting the day directory under an open writer. Without the fix that test hangs. What made the worker fail in that run is unknown, because its server log was lost. None of the eight later runs hit it.

## Agent search over 90 days

Measured on 2026-11-23 with `bench/agent-search.sh`, the defaults: 18M logs spread evenly over 90 days in 4 projects (`examples/fill_logs.rs`), 20,000 issues with 20 events each over the same days, 20 timed calls per case after one first call. It calls the MCP tools over HTTP the way an agent does. The data was just written, so the OS still cached it, and a first call measures opening the day indexes, not a cold disk. The shared dev Mac had a load average of 9 to 10 throughout.

The store held 3.3 GB of logs in 91 day indexes, at most 31 files per day, and 80 MB of SQLite. Writing the logs took 204.6 s, 88k rows/s.

The last column is the same case before day indexes stayed open between searches (the run before, same data and machine).

| Tool | Case | Rows | First | p50 | p95 | p50 before |
|---|---|---|---|---|---|---|
| `search_logs` | last hour, newest 100 | 100 | 3.7 ms | 1.6 ms | 1.8 ms | 2.3 ms |
| `search_logs` | last 24 h, `timeout` | 100 | 5.9 ms | 1.9 ms | 2.1 ms | 3.3 ms |
| `search_logs` | last 7 days, errors | 100 | 2.7 ms | 2.0 ms | 2.1 ms | 3.4 ms |
| `search_logs` | 30 days, two attributes | 100 | 3.5 ms | 2.3 ms | 2.5 ms | 3.7 ms |
| `search_logs` | one old day, 80 days back | 100 | 3.0 ms | 1.5 ms | 1.7 ms | 3.5 ms |
| `search_logs` | 90 days, one request ID logged 60 days ago | 1 | 792.3 ms | 2.3 ms | 2.9 ms | 87.9 ms |
| `search_logs` | 90 days, one trace ID logged 50 days ago | 1 | 216.6 ms | 2.7 ms | 3.0 ms | 92.6 ms |
| `search_logs` | 90 days, a rare hostname part | 260 | 184.5 ms | 6.0 ms | 6.9 ms | 101.3 ms |
| `search_logs` | 90 days, no hit | 0 | 19.8 ms | 2.6 ms | 2.8 ms | 86.3 ms |
| `sql` | open issues by last seen | 20 | 0.8 ms | 0.7 ms | 0.8 ms | 0.6 ms |
| `sql` | issue titles `LIKE '%timed out%'` | 50 | 0.7 ms | 0.7 ms | 0.7 ms | 0.7 ms |
| `sql` | one issue, events per day | 15 | 1.0 ms | 0.6 ms | 0.7 ms | 0.6 ms |
| `sql` | events per day over all 400k | 91 | 95.4 ms | 95.9 ms | 101.3 ms | 102.8 ms |
| `sql` | events per release in production | 100 | 97.8 ms | 95.8 ms | 99.9 ms | 102.8 ms |
| `issue_context` | newest unresolved issue | 1 | 1.2 ms | 0.8 ms | 1.0 ms | 1.7 ms |

- **Every log search answers in under 7 ms at p50 and p95, over any window of the 90 days.** A query that fills its limit from recent days opens few days, because search walks the days newest first and stops at `limit` hits. A rare needle has to search all 91 days, which now costs at most about 0.03 ms per day (inferred from 2.3 ms for 91 days, HTTP included).
- **Opening a day costs about 1 ms, so the server keeps days open.** Before, every search opened each day it touched, and a needle over 90 days took 86 to 101 ms at p50. Now a day stays open until its `meta.json` changes, which only today's day does, and the needle takes 2.3 to 6.0 ms. The first search after the server starts still opens every day it needs: 184 to 792 ms for the 90-day cases. The no-hit case came after them and opened nothing new (19.8 ms). The rows of the rare hostname part differ between runs (281 and 260) because the hostname is picked at random per run.
- **At most 400 days stay open**, the least recently searched closed first. Each open day maps its files, at most 31 here, so 400 days stay near 12,400 maps, under Linux's default limit of 65,530 per process (`vm.max_map_count`).
- **The benchmark found a search bug.** In the first run the request ID and hostname cases returned 0 rows. A bare word was parsed against the `attributes` JSON field, which matches only its root path, so no attribute value was ever found by a bare word. The `values` field fixed it (see the log storage section in `docs/design.md`). It costs 32% more disk (2.5 GB before, 3.3 GB after) and 21% more write time (172.1 s before, 208.9 s after). Query latency stayed within noise of the first run (measured).
- **SQL over 400k events is about 100 ms when it reads every event.** The events table has no index for a group by day or release, and an agent asking that once per triage is fine. Indexed lookups stay under 1 ms.

## Footprint for the forecast

Measured on 2026-11-25 with `bench/footprint.sh` at commit `469fd53` (`v0.1.0-172-g469fd53`), at normal priority, which `bench/footprint.json` records as `machine.priority`. The coefficients are in `bench/footprint.json` (schema `davy.footprint`, version 1), with the raw numbers per run. The binary does not read the file. The log forecast of `usage`, `davy usage` and `davy doctor` carries four of its fields as constants in `src/usage/footprint.rs`: `coefficients.logs.disk_per_raw_real` `min`, `median` and `max` as the low, middle and high ratio of disk to raw bytes, and `coefficients.logs.extra_bytes_per_day_index.median` as the bytes a first day adds for its day index. The answer names the run as `footprint`. The test `footprint_constants_match_the_bench_report` reads the file and fails when a constant differs from it, when one is 0, or when the schema or version changes, so a new run of the benchmark means new constants in the same commit. How the forecast blends them with what a server measures is in the forecast section of `docs/design.md`. Two earlier runs are compared. The "first run" was at normal priority and commit `f34f3cd`, before the timestamps were fixed to whole days (`git show 06fbcbe:bench/footprint.json`). The "slow run" was under `taskpolicy -b` at commit `b54aaa7` (`git show da8ed9e:bench/footprint.json`). Since the slow run, main gained the log pattern counters, usage accounting (`usage_daily`), retention fixes and the `latest_payloads` table for the newest payload of an issue (migration 19).

**Machine.** A shared dev Mac: Mac14,5, Apple M2 Max, 12 cores, 32 GB RAM, macOS 26.5.1, APFS on the internal SSD. Other agents were working, but no build ran next to the benchmark. The load average was 5.2 at the start and 3.7 at the end, against 13 to 19 for the first run and 13 to 23 for the slow run. `bench/footprint.json` records it per run, and lists the noisy fields under `noisy`. Runs on reference VPS sizes (vCPU, RAM, disk) are still to come.

**How far to trust the numbers.**

- **The segment count depends on timing (from the code).** The log writer commits every second or every 100k rows, so a slower ingest makes more, smaller segments, and its merges leave more of them. Each segment repeats some fixed cost.
- **Priority explains most of the slow run's extra disk (inferred).** This run left about as many segments as the first run: hdfs-2m 10 against 7 (20 in the slow run), spark-1m 5 against 4 (17). Disk / raw fell back from a median of 0.76× to 0.68×. One segment / raw, without the pattern counters, stayed within 0% to 5% of the slow run for the large corpora, nginx-parsed aside (below), so priority did not cause that part of the rise. It stays 2% to 10% above the first run, apache most, and its cause is still not isolated: whole-day timestamps, the scrubbing change and other code all came in between. This run also differs from the slow run in 122 commits, so no pair of runs isolates priority.
- **A run at the same code an hour earlier gave the same Disk / raw within 1% for every corpus (measured).** It ran before the counting of `latest_payloads` and the pattern files was committed, at load 4.6 to 2.9. Its forced merge left OpenSSH at 0.67× instead of 0.70×, and synthetic-2m kept 10 segments instead of 17 at the same Disk / raw. So One segment / raw and the segment count move by a few percent between runs (noisy).
- **CPU, time and memory numbers are noisy** and only give the order of magnitude.

### Method

- **Corpora.** Six real corpora of 25k lines or more in seven runs (nginx raw and parsed), the synthetic rows, and five small samples. The Loghub 2k samples of HDFS, Apache, OpenSSH, Linux and Spark. The full Loghub files from Zenodo record 8196385 where the download is reasonable: all of Apache, Linux and OpenSSH, the first 1M lines of Spark and the first 2M of HDFS (3.7M lines in total). The public nginx access log sample of `elastic/examples` at a pinned commit (51,462 lines), sent once raw and once parsed. 2M rows of the synthetic pino generator (`examples/shared/pino.rs`, shared with `examples/load_logs.rs`), for continuity with the sections above. The script downloads each file to `.scratch/footprint/corpora` in the main checkout over HTTPS only, keeps it only if its SHA-256 matches, and unpacks an archive to stdout only. A corpus cut from an archive has a SHA-256 of its own, checked before every use. Nothing from a download is executed.
- **Ingest.** Each corpus goes through `davy serve` over the Logtail route, gzip NDJSON in batches of 1000 from 4 connections, so the default scrubbing, parsing, pattern counting, commits and merges apply. A raw log line is sent as `message`. `nginx-parsed` splits each access log line into attributes (status, user agent and so on) the way a log shipper would. Bytes that are not UTF-8 become U+FFFD.
- **Timestamps.** The lines carry no usable time, so each gets a synthetic one, spaced evenly over the whole UTC day before the run, or over the 90 whole days before it for `hdfs-2m` and `synthetic-2m`. So the bytes do not depend on the hour a run starts. `ts` is about a quarter of a raw log index of 25k lines or more (19% to 27% in the default one-day runs, 22% for nginx with IPs kept, 21% for hdfs-2m over 90 days, 9% to 10% for the synthetic rows), and real timestamps come in bursts, which may compress differently (not measured).
- **IP scrubbing.** The default scrub policy replaces each IP address with `[Filtered: ip]`, and nginx and OpenSSH lines are full of them, so their default ratios include that. Both also run with IP scrubbing off (`IP kept` in the table).
- **Disk.** `du` of the log directory after the server committed, ran its own merges and stopped (Disk / raw), then after a forced merge of each day into one segment (One segment / raw). The pattern counters, one `patterns-<project>.json` per project in each day directory, are part of that directory, so both ratios include them. Each run records their files, bytes and allocated disk (`patterns_files`, `patterns_bytes`, `patterns_disk_bytes`). Raw is the bytes of the lines as read, each with its newline. The share of a field (`values`) comes from Tantivy's space usage and counts against the index, docstore included.
- **Peaks.** Disk is sampled every 0.2 s while the server ingests and while the forced merge runs. The ingest peak is the highest sample above the size after the server stopped, the merge peak the highest sample above the size before the merge, so each is at least that. A small corpus finishes before the sampler sees a peak, so the table shows the ingest peak only for corpora of 500k lines or more, and the merge headroom counts only those whose forced merge ran 2 s or more.
- **CPU and memory.** `/usr/bin/time -l` around the server gives CPU seconds, peak RSS and peak memory footprint, for ingest and, in a second server start, for searches. CPU µs per row is the server's CPU over the stored rows. RSS counts the pages of the index files the server mapped and read, and on a machine with 32 GB and a warm page cache those stay resident. The footprint, macOS's measure of the memory a process owns, leaves out such clean file pages, so it is closer to what the server needs. After the ingest of a 90-day corpus the script restarts the server and, through MCP, runs two light queries per window, a search with no hit and a search for the newest 100 rows, over the last 7, 30 and 90 days, reading RSS and footprint after each window.
- **Retention.** The 90-day corpora then lose all but 30 days in one retention pass of the log writer (`LogWriter::start` with 30 days), timed with what it frees. The pass must leave exactly the 30 days it should, or the run fails.
- **Errors.** A captured Node.js event (`e2e/fixtures/sentry-node-10/4.body`, 2747 bytes of JSON) is sent as 100k envelopes over 7 days: 5,000 issues of 20 events, and 10 issues of 10,000 events. Payload sampling is on, as always. `dbstat` gives the bytes of each table with its indexes, and a `VACUUM INTO` copy gives the same rows compacted. A payload counts whether it sits in `events` or in `latest_payloads`.

### Logs

| Corpus | Lines | Raw B/line | Disk / raw | Disk B/line | One segment / raw | Segments | `values` | Ingest peak extra disk | CPU µs/row | Peak RSS | Peak footprint |
|---|---|---|---|---|---|---|---|---|---|---|---|
| apache | 56,482 | 91 | 0.80× | 73 | 0.80× | 1 | 1% |  | 7.6 | 107 MB | 84 MB |
| hdfs-2m | 2,000,000 | 139 | 0.62× | 86 | 0.60× | 10 | 1% | 36 MB | 12.0 | 302 MB | 174 MB |
| linux | 25,567 | 92 | 1.03× | 94 | 1.03× | 1 | 1% |  | 8.2 | 79 MB | 58 MB |
| nginx | 51,462 | 136 | 0.55× | 74 | 0.55× | 1 | 1% |  | 9.7 | 109 MB | 86 MB |
| nginx, IP kept | 51,462 | 136 | 0.61× | 82 | 0.61× | 1 | 1% |  | 9.3 | 102 MB | 82 MB |
| nginx-parsed | 51,462 | 136 | 0.67× | 91 | 0.67× | 1 | 23% |  | 18.6 | 98 MB | 73 MB |
| openssh | 655,147 | 112 | 0.68× | 77 | 0.70× | 8 | 1% | 0 MB | 8.9 | 169 MB | 112 MB |
| openssh, IP kept | 655,147 | 112 | 0.71× | 80 | 0.72× | 8 | 1% | 0 MB | 8.5 | 157 MB | 106 MB |
| spark-1m | 1,000,000 | 105 | 0.75× | 79 | 0.74× | 5 | 1% | 37 MB | 10.6 | 262 MB | 133 MB |
| synthetic-2m | 2,000,000 | 260 | 0.71× | 185 | 0.71× | 17 | 20% | 97 MB | 26.9 | 544 MB | 286 MB |
| the 5 small samples | 2,000 each | 86 to 144 | 0.96× to 1.17× | 82 to 160 | 0.96× to 1.17× | 1 | 1% |  | 15.0 to 30.0 | 39 to 41 MB | 24 to 26 MB |

Disk / raw in the first run: apache 0.69×, hdfs-2m 0.59×, linux 0.88×, nginx 0.52×, nginx-parsed 0.65×, openssh 0.65×, spark-1m 0.71×, synthetic-2m 0.69×, the 2k samples 0.91× to 1.10×. It had no `IP kept` runs. In the slow run: apache 0.80×, hdfs-2m 0.67×, linux 1.00×, nginx 0.56×, nginx with IPs kept 0.62×, nginx-parsed 0.76×, openssh 0.74×, openssh with IPs kept 0.72×, spark-1m 0.82×, synthetic-2m 0.76×, the 2k samples 0.82× to 1.10×.

Over 90 days:

| Corpus | Disk / raw | Day indexes | Segments | RSS (footprint) after start | after 7 days | after 30 days | after 90 days | Retention to 30 days |
|---|---|---|---|---|---|---|---|---|
| hdfs-2m | 0.64× | 90 | 136 | 19 MB (7.0 MB) | 25 MB (8.4 MB) | 30 MB (8.9 MB) | 44 MB (10.2 MB) | 90 to 30 days, 116 MB freed in 0.06 s |
| synthetic-2m | 0.80× | 90 | 125 | 19 MB (7.0 MB) | 24 MB (7.8 MB) | 31 MB (8.3 MB) | 48 MB (9.7 MB) | 90 to 30 days, 276 MB freed in 0.06 s |

- **Real logs take 0.55× to 1.03× of their raw size, 79 bytes per line at the median (measured).** The seven runs of 25k lines or more give 0.68× at the median, against 0.65× and 74 bytes in the first run and 0.76× and 86 bytes in the slow run. Logs with long repeated text compress best: nginx and HDFS at 0.55× and 0.62×, Linux syslog at 1.03× in its 25k lines, 9% of which is its pattern counters. The synthetic pino rows take 0.71×, in line with the real logs.
- **Pattern counters take 4 to 274 KB per project and day (measured).** A corpus with few patterns (nginx, the synthetic rows) fills one 4 KB block. Linux syslog wrote 221 KB for 25k lines and OpenSSH 274 KB for 655k lines, 9.2% and 0.5% of their disk. The 2k samples wrote 4 to 41 KB, 2.5% to 16% of their disk. Over 90 days a file per day cost 8 and 4 KB on average (hdfs-2m 717 KB, synthetic-2m 369 KB). Without them the large real corpora keep the same median, 0.68× and 79 bytes per line. The file grows with the patterns a day has, not with its lines. It holds at most 1,000 patterns (`MAX_PATTERNS` in `src/logs/tally.rs`). How many bytes that allows was not measured.
- **IP scrubbing makes nginx and OpenSSH smaller.** With IP addresses kept, nginx takes 0.61× instead of 0.55×, because `[Filtered: ip]` repeats where the addresses did not. OpenSSH, with fewer addresses per line, takes 0.71× instead of 0.68×, both with 8 segments (one segment: 0.72× instead of 0.70×). A forecast for a project that turns IP scrubbing off should use the `IP kept` ratios for logs like these.
- **A small corpus costs more per line.** The 2k samples take 0.96× to 1.17×, because a day index has a fixed cost and the docstore compresses better in larger blocks (from how the index is built, not measured), and because the pattern counters weigh more in a small day (measured). Without the counters they take 0.87× to 1.07×, between the first run and the slow run. The forecast should use the large corpora.
- **A forced merge into one segment changed the large corpora by at most 3% in either direction (measured).** hdfs-2m went from 0.62× to 0.60×, OpenSSH from 0.68× to 0.70×. In the run an hour earlier OpenSSH went from 0.68× to 0.67× instead (noisy). The corpora of 56k lines or fewer ended in one segment, so the merge left them as they were. Davy never forces such a merge, so the forecast can use Disk / raw as is, since it is what Davy leaves on disk.
- **Parsed attributes cost 23% more.** The same nginx log takes 0.55× as one message and 0.67× split into attributes (24% in the first run). Each attribute value is indexed twice (from the code), as a JSON path in `attributes` and as a bare word in `values` (23% of the index). The synthetic rows, all attributes, give `values` 20%. A raw line has no attributes, so `values` stays at about 1%, its cost per document. The slow run's 0.76× for nginx-parsed, with 6 segments, did not come back: this run left one segment.
- **Spreading the same lines over 90 days costs 0.07 to 0.49 MB per extra day index (measured, two corpora).** hdfs-2m went from 0.62× in one day to 0.64× in 90, synthetic-2m from 0.71× to 0.80×. The first run measured 0.1 to 0.6 MB, the slow run 0.03 to 1.2 MB. The pattern counters are 8 and 4 KB of that per day. If the cost follows the segments a day keeps, it varies with the ingest rate too (inferred).
- **Disk needs headroom for merges.** While ingesting, the log directory rose by at least 0 to 0.46× above its final size (median 0.24×; the first run saw up to 0.52×). A forced merge into one segment needed 0.89× and 1.00× of the day, from hdfs-2m and synthetic-2m: the OpenSSH and Spark merges took 1.1 and 1.5 s, too short for the sampler. Davy's own merges rewrite only some segments at a time, so 1× of the largest day index is a safe reserve (inferred), and the synthetic merge came within 0.4% of it.
- **Search memory barely grows with the days opened.** The footprint went from 7.0 MB at start to 9.7 and 10.2 MB after opening 90 days, 0.03 to 0.04 MB per open day. RSS grew from 19 MB to 44 and 48 MB, but that growth is index pages the queries read, which the OS can drop. These are two light queries per window; a query that reads many documents of every day would touch more pages. The first search with no hit over 90 days took 16 to 18 ms, against 23 to 36 ms in the first run and 533 to 537 ms in the slow run (noisy).
- **Ingest memory: 106 to 286 MB of footprint for 655k lines or more (noisy).** That is the peak footprint of the default and `IP kept` one-day runs of 655k lines or more, highest for the 260-byte synthetic rows, against 24 to 26 MB for 2k lines. Peak RSS was 51 to 258 MB above it, against 5 to 30 MB in the slow run. RSS came back to the first run's level (169 to 544 MB against 184 to 540 MB), so the slow run held fewer index pages resident, why is not measured. What the footprint holds (writer buffers, the ingest queue, merges, pattern counters) is not measured either.
- **Ingest cost 7.6 to 26.9 µs of CPU per row, 10.2 at the median (noisy).** A raw line cost 7.6 to 12.0 µs, a parsed nginx line 18.6 µs and a synthetic JSON row 26.9 µs. The first run gave 6.5 to 27.6 µs (median 9.4), the slow run 22 to 166 µs (median 38.2). Each raw-line corpus cost 4% to 16% more than in the first run, the synthetic rows 3% less. The pattern counting that came in since may be part of that, but the first run had three times the load, and the data cannot tell.
- **Retention is far inside its time budget.** Removing 60 day indexes took 0.06 s and freed 116 and 276 MB, against 0.52 and 0.86 s in the slow run and 0.05 to 0.08 s in the first run. A day past retention is renamed into `.trash` and removed whole, with its pattern counters. The log writer gives one retention pass a budget of 1 s (`RETENTION_BUDGET` in `src/logs/store.rs`) and leaves what remains to the next pass, and a pass that stops early fails this benchmark. The data ends the day before the run, so 30 day indexes remain.

### Errors

| Issues × events | SQLite B/event | Row B/event | Compacted row B/event | Payloads/issue | B/payload | B/hourly row | B/tag row | B/issue | CPU µs/event |
|---|---|---|---|---|---|---|---|---|---|
| 10 × 10,000 | 271 | 244 | 222 | 150 | 1570 | 44 |  |  | 262 |
| 5,000 × 20 | 2237 | 582 | 571 | 20 | 1572 | 37 | 66 | 600 | 278 |

Row is the bytes of an event in the `events` and `latest_payloads` tables and their indexes, without its payload. A payload is the zstd event JSON, 0.57× of the 2747 bytes the SDK sent. An issue keeps the payload of its first 50 events, of every 100th after and of its newest. When the newest is outside that sample, its payload sits in `latest_payloads`, one row per issue. In both runs the last event of each issue was in the sample, so `latest_payloads` ended empty. Each event of the first run fell in an hour of its own, so each made an hourly row. The issue and tag bytes come from the 5,000-issue run only, since 10 issues fit in a few 4 KB pages. Every figure here is for this one event shape; a larger event has a larger payload and scales the payload bytes with it.

- **An event that keeps its payload costs its payload plus about 580 bytes (measured).** The cold-issue run kept every payload, as each issue had only 20 events. That is 20 bytes more than in the two earlier runs (562 and 563). The `events_issue_timestamp` index became a migration after the slow run, and an entry of an issue id, a timestamp and a row id fits about that size (inferred from the schema, not isolated).
- **An event of a hot issue now takes 244 bytes as written and 222 compacted (measured).** The whole database was 27 MB as written and 25 MB after `VACUUM`, against 211 and 23 MB in the slow run, 8 times smaller. Free space inside the pages of `events` fell from 92% to 4%. Before, the newest event of an issue kept its payload in its `events` row until a newer event arrived, and the space it left stayed empty. The forecast can count an event of a hot issue at about 244 bytes plus its share of the kept payloads, no longer like one that keeps its payload.
- **Error ingest cost 262 to 278 µs of CPU per event (noisy).** 285 to 302 µs in the first run, 1,152 to 1,248 µs in the slow run. The client sent one event per envelope, so this includes HTTP and envelope parsing for every event. Peak footprint was 12 MB, peak RSS 25 MB.

## Error events of hot issues

Measured on 2026-11-24 with `examples/error_footprint.rs` through the real writer, on an M2 Max (32 GB) with a load average of 15 to 25: 10 issues of 10,000 Sentry Node events each, interleaved, past two rounds of retention. "Payloads of hot issues" in `docs/design.md` has the designs compared, the reads and the upgrade.

| | Bytes per event, 1 event per transaction | Bytes per event, 64 per transaction | Writer, µs per event, 64 per transaction |
|---|---|---|---|
| Newest payload in its `events` row, cleared later | 2,169 | 573 | 64.0 |
| Newest payload in `latest_payloads` | 271 | 271 | 59.9 |

The store compacts to 241 bytes per event (measured). Reading an issue's newest payload went from 5.4 to 8.1 µs and a page of 50 events from 25.1 to 28.3 µs (median of 15 rounds).

Rerun on 2026-11-25 with the code of `469fd53` at a load average of 2.2 to 3.0 (measured): `store` gave 271 bytes per event at 1 and at 64 events per transaction, compacting to 241. The writer took 54.6 µs per event at 64 per transaction and 159.4 µs at 1 per transaction, where the commit's fsync dominates. The old layout no longer runs through the writer. As bare SQL (`sim current`) it gave 2,168 and 572 bytes per event, the same as the writer gave before.

## Cold tier

Measured on 2026-11-27 with `examples/cold_bench.rs` on the corpora of the footprint benchmark, on the same M2 Max, at background priority (`taskpolicy -b`) with other agents working, load average 4.1 to 14.8. Background priority slows the CPU numbers, not the sizes. "Cold tier" in `docs/design.md` has the format and how a day turns cold.

### Method

- Each corpus goes through the log writer into one day index, as `line` (each line a message), `nginx` (an access log line split into attributes, like `nginx-parsed` above) or `json` (a synthetic pino row). The size of that index is Hot.
- The day is then converted with each zstd level (3, 9, 19), block size (256 KiB, 1 MiB, 4 MiB) and with or without a dictionary trained on the day (112 KiB from 20,000 samples). Spark, HDFS and the synthetic rows ran without a dictionary, as it lost on every smaller corpus, and the synthetic rows at level 19 with 1 MiB blocks only, as level 19 took minutes per setting.
- Disk is the allocated size of the files. Convert is the time to read the day index and write the files, on one thread. A full scan decompresses every block and reads every row. A text query also matches each row the way the day index would (see "Search over cold days" in `docs/design.md`), with no time budget.

### Results

At the chosen setting, level 9 with 1 MiB blocks and no dictionary:

| Corpus | Lines | Hot / raw | Cold / raw | Cold / hot | Convert rows/s | Full scan | Text query |
|---|---|---|---|---|---|---|---|
| apache | 56,482 | 0.88× | 0.050× | 0.057 | 166k | 8 ms | `error` 472 ms (38,081 hits) |
| linux | 25,567 | 1.08× | 0.071× | 0.066 | 139k | 4 ms | `authentication failure` 92 ms (4,094 hits) |
| nginx-parsed | 51,462 | 1.17× | 0.054× | 0.047 | 53k | 8 ms | `attributes.status:404` 1,545 ms (33,876 hits) |
| openssh | 655,147 | 0.95× | 0.056× | 0.059 | 131k | 132 ms | `"Failed password"` 3,941 ms (218,071 hits), `webmaster` 283 ms (36) |
| spark-1m | 1,000,000 | 0.77× | 0.059× | 0.076 | 161k | 216 ms | `Exception` 527 ms (408 hits) |
| hdfs-2m | 2,000,000 | 0.73× to 0.76× | 0.098× | 0.129 to 0.135 | 96k to 98k | 603 to 674 ms | `PacketResponder` 7,481 ms (633,032 hits), the block id 1,002 ms (269) |
| synthetic-2m | 2,000,000 | 0.75× to 0.76× | 0.121× | 0.159 to 0.162 | 46k | 648 to 817 ms | `"database timeout"` 4,442 to 4,534 ms (60,209 hits) |

HDFS and the synthetic rows ran twice, the second time after a change to the word filter, so both runs are given; the day index came out up to 4.2% smaller in the second run, the cold files the same to the byte.

The synthetic rows across settings, without a dictionary:

| Level, block | Cold / raw | Cold / hot | Convert rows/s | Full scan |
|---|---|---|---|---|
| 3, 256 KiB | 0.163× | 0.214 | 57k | 874 ms |
| 3, 1 MiB | 0.163× | 0.214 | 62k | 891 ms |
| 3, 4 MiB | 0.162× | 0.213 | 65k | 872 ms |
| 9, 256 KiB | 0.129× | 0.169 | 49k | 810 ms |
| 9, 1 MiB | 0.121× | 0.159 | 46k | 817 ms |
| 9, 4 MiB | 0.122× | 0.160 | 49k | 902 ms |
| 19, 1 MiB | 0.117× | 0.153 | 6.9k | 918 ms |

- **Cold files take 0.05× to 0.12× of the raw bytes, 0.05× to 0.16× of the day index (measured).** Logs with long repeated text pack best, as in the day index. The synthetic rows, all attributes, pack worst, and set the prior of the forecast: 0.2, the largest cold-to-hot ratio, 0.162, with a margin.
- **Level 9 over level 3.** Level 9 took 13% to 26% less disk on the large corpora (hdfs 0.098× against 0.113×, spark 0.059× against 0.079×, synthetic 0.121× against 0.163×) and converted at 46k to 166k rows/s against 62k to 280k. A day of 2M rows converts in about 20 to 45 s on one background thread, far inside a day.
- **Not level 19.** Its files were up to 17% smaller than level 9's, 3% for the synthetic rows, and it converted 7 to 21 times slower: 4k to 13k rows/s, 260 to 292 s for a day of 2M rows.
- **1 MiB blocks.** Against 256 KiB they took 2% to 16% less disk at level 9 on the large corpora. 4 MiB saved at most 5% more (hdfs 0.093× against 0.098×), and in the synthetic rows nothing, while a search that needs one row decompresses 4 times as much and a fetch by byte range from object storage reads 4 times as much.
- **No dictionary.** A dictionary trained per file is stored in that file, and in every corpus measured it took more disk than it saved: apache 0.071× against 0.050× at level 9, linux 0.113× against 0.071×, nginx-parsed 0.068× against 0.054×, openssh 0.057× against 0.056×. It also converted 2 to 4.5 times slower on the three small corpora.
- **A full scan reads 2.4 to 7.0M rows a second at the chosen setting (noisy).** A text query reads as fast while the word filter leaves few rows; rows it lets through are indexed in memory to run the query, at about 30k to 90k rows a second, so `PacketResponder`, held by a third of HDFS, takes 7.5 s, and `"Failed password"` on OpenSSH 3.9 s. The 5 s budget of a search stops such a scan and says where.
- **The word filter matters for phrases.** In the first run the block id phrase of HDFS took 37.6 s: in a query over two fields the filter took each field's first word, `blk`, which every line holds. It now takes the list whose shortest word is longest, and the same query takes 1.0 s.

### Ingest during a conversion

`examples/cold_ingest.rs` writes 1M synthetic web logs into a day ten days back, then starts a log writer with 7 hot days and ingests logs for today in batches of 5,000 as fast as the queue takes them until that day is cold, then as long again with every day hot. At background priority, load average 5.4 to 5.6:

| | Rows | Seconds | Rows/s |
|---|---|---|---|
| While the day turned cold | 1,055,000 | 19.1 | 55.2k |
| Every day hot | 1,075,000 | 19.1 | 56.2k |

Ingest kept 0.98 of its rate while the conversion ran on its own thread. The conversion of 1M rows, three projects, ended within the 19.1 s.

## Rerun

```sh
cd bench/storage
cargo build --release
./run.sh > results.txt 2>&1      # about 45 minutes; data goes to ../../.scratch/bench-data
cd ../..
bash bench/load-logs.sh 1000000 1000 4   # logs ingest through the server, about 1 minute
bash bench/agent-search.sh               # agent search over 90 days, about 5 minutes and 4 GB of scratch
bench/footprint.sh                       # footprint coefficients, macOS only, at normal priority, about 3 minutes after the build; 1.3 GB of downloads and corpora
                                         # `quick` for the 2k samples only, written to the work directory
                                         # a full run refuses background priority unless FOOTPRINT_LOW_PRIORITY=1
                                         # FOOTPRINT_WORK, FOOTPRINT_OUT, FOOTPRINT_SHARED, FOOTPRINT_LOW_PRIORITY: see the script's header
python3 -I bench/footprint-report.py --tables bench/footprint.json   # the footprint tables above, from the committed report
cargo build --release --example error_footprint
target/release/examples/error_footprint store .scratch/hot 10 10000 64   # error disk per event, about 15 s
target/release/examples/error_footprint sim side .scratch/side 10 10000 64   # one design as bare SQL: current, table, side or autovacuum
cargo build --release --example cold_bench --example cold_ingest
target/release/examples/cold_bench CORPUS line .scratch/cold 'Failed password'   # cold tier settings; COLD_LEVELS, COLD_BLOCKS, COLD_DICTS and COLD_QUERY_AT narrow the run
target/release/examples/cold_ingest .scratch/ingest 1000000                    # ingest while a day of 1M logs turns cold, about 1 minute
```
