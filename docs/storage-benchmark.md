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

Measured on 2026-11-24 with `bench/footprint.sh` at commit `b54aaa7` (`v0.1.0-50-gb54aaa7`), under `taskpolicy -b`. The coefficients are in `bench/footprint.json` (schema `davy.footprint`, version 1), which `davy forecast` is meant to read. Raw numbers per run are kept there too. The last column of the logs table and the comparisons marked "previous run" come from an earlier run at normal priority, at commit `f34f3cd`, before the timestamps were fixed to whole days.

**Machine.** A shared dev Mac: Mac14,5, Apple M2 Max, 12 cores, 32 GB RAM, macOS 26.5.1, APFS on the internal SSD. Other agents ran builds throughout, and the load average was 13 to 23 at the start of the runs. `bench/footprint.json` records it per run, and lists the noisy fields under `noisy`. Runs on reference VPS sizes (vCPU, RAM, disk) are still to come.

**How far to trust the numbers.**

- **The bytes of one index are reliable, the segment count is not.** Everything that ends up in a segment depends only on the data and the code. How many segments Davy leaves behind depends on timing: the log writer commits every second or every 100k rows, so a slower ingest makes more, smaller segments, and its merges leave more of them. Each segment repeats some fixed cost.
- **This run was slow.** `taskpolicy -b` puts the process at background priority, which on Apple Silicon means the efficiency cores and throttled I/O. CPU per row came out 3 to 6 times the previous run, hdfs-2m kept 20 segments instead of 7, and Disk / raw came out 8 to 24% higher. One segment / raw moved less for the large corpora, 2% to 7% higher, except nginx-parsed at 24% higher. The 2k samples, one or two segments each, came out 16 to 20 KB smaller per day index than in the previous run (cause not isolated). So Disk / raw from this run is the upper end, a server that ingests slowly. A forecast that wants a safe figure can use it as is.
- **CPU, time and memory numbers are noisy** and only give the order of magnitude, here more than usual.

### Method

- **Corpora.** Six real corpora of 25k lines or more in seven runs (nginx raw and parsed), the synthetic rows, and five small samples. The Loghub 2k samples of HDFS, Apache, OpenSSH, Linux and Spark. The full Loghub files from Zenodo record 8196385 where the download is reasonable: all of Apache, Linux and OpenSSH, the first 1M lines of Spark and the first 2M of HDFS (3.7M lines in total). The public nginx access log sample of `elastic/examples` at a pinned commit (51,462 lines), sent once raw and once parsed. 2M rows of the synthetic pino generator (`examples/shared/pino.rs`, shared with `examples/load_logs.rs`), for continuity with the sections above. The script downloads each file to `.scratch/footprint/corpora` in the main checkout over HTTPS only, keeps it only if its SHA-256 matches, and unpacks an archive to stdout only. A corpus cut from an archive has a SHA-256 of its own, checked before every use. Nothing from a download is executed.
- **Ingest.** Each corpus goes through `davy serve` over the Logtail route, gzip NDJSON in batches of 1000 from 4 connections, so the default scrubbing, parsing, commits and merges apply. A raw log line is sent as `message`. `nginx-parsed` splits each access log line into attributes (status, user agent and so on) the way a log shipper would. Bytes that are not UTF-8 become U+FFFD.
- **Timestamps.** The lines carry no usable time, so each gets a synthetic one, spaced evenly over the whole UTC day before the run, or over the 90 whole days before it for `hdfs-2m` and `synthetic-2m`. So the bytes do not depend on the hour a run starts. `ts` is about a quarter of a raw log index of 25k lines or more (23% to 28%, 10% for the synthetic rows), and real timestamps come in bursts, which may compress differently (not measured).
- **IP scrubbing.** The default scrub policy replaces each IP address with `[Filtered: ip]`, and nginx and OpenSSH lines are full of them, so their default ratios include that. Both also run with IP scrubbing off (`IP kept` in the table).
- **Disk.** `du` of the log directory after the server committed, ran its own merges and stopped (Disk / raw), then after a forced merge of each day into one segment (One segment / raw). Raw is the bytes of the lines as read, each with its newline. The share of a field (`values`) comes from Tantivy's space usage and counts against the index, docstore included.
- **Peaks.** Disk is sampled every 0.2 s while the server ingests and while the forced merge runs. Peak extra disk is the highest sample above the final size, so it is at least that, from four runs. A small corpus finishes before the sampler sees a peak, so only corpora of 500k lines or more count for it.
- **CPU and memory.** `/usr/bin/time -l` around the server gives CPU seconds, peak RSS and peak memory footprint, for ingest and, in a second server start, for searches. CPU µs per row is the server's CPU over the stored rows. RSS counts the pages of the index files the server mapped and read, and on a machine with 32 GB and a warm page cache those stay resident. The footprint, macOS's measure of the memory a process owns, leaves out such clean file pages, so it is closer to what the server needs. After the ingest of a 90-day corpus the script restarts the server and, through MCP, runs two light queries per window, a search with no hit and a search for the newest 100 rows, over the last 7, 30 and 90 days, reading RSS and footprint after each window.
- **Retention.** The 90-day corpora then lose all but 30 days in one retention pass of the log writer (`LogWriter::start` with 30 days), timed with what it frees. The pass must leave exactly the 30 days it should, or the run fails.
- **Errors.** A captured Node.js event (`e2e/fixtures/sentry-node-10/4.body`, 2747 bytes of JSON) is sent as 100k envelopes over 7 days: 5,000 issues of 20 events, and 10 issues of 10,000 events. Payload sampling is on, as always. `dbstat` gives the bytes of each table with its indexes, and a `VACUUM INTO` copy gives the same rows compacted.

### Logs

| Corpus | Lines | Raw B/line | Disk / raw | Disk B/line | One segment / raw | Segments | `values` | Peak extra disk | CPU µs/row | Peak RSS | Peak footprint | Disk / raw, previous run |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| apache | 56,482 | 91 | 0.80× | 73 | 0.74× | 4 | 2% | | 22.5 | 91 MB | 76 MB | 0.69× |
| linux | 25,567 | 92 | 1.00× | 92 | 0.90× | 2 | 1% | | 27.0 | 68 MB | 49 MB | 0.88× |
| openssh | 655,147 | 112 | 0.74× | 83 | 0.67× | 7 | 1% | 47 MB | 42.0 | 158 MB | 128 MB | 0.65× |
| openssh, IP kept | 655,147 | 112 | 0.72× | 81 | 0.70× | 17 | 1% | 37 MB | 37.3 | 131 MB | 125 MB | |
| spark-1m | 1,000,000 | 105 | 0.82× | 86 | 0.74× | 17 | 1% | 55 MB | 34.4 | 146 MB | 131 MB | 0.71× |
| hdfs-2m | 2,000,000 | 139 | 0.67× | 93 | 0.58× | 20 | 1% | 125 MB | 56.5 | 172 MB | 142 MB | 0.59× |
| nginx | 51,462 | 136 | 0.56× | 77 | 0.54× | 3 | 1% | | 30.3 | 91 MB | 76 MB | 0.52× |
| nginx, IP kept | 51,462 | 136 | 0.62× | 84 | 0.60× | 3 | 1% | | 30.1 | 88 MB | 71 MB | |
| nginx-parsed | 51,462 | 136 | 0.76× | 103 | 0.81× | 6 | 24% | | 51.3 | 102 MB | 86 MB | 0.65× |
| synthetic-2m | 2,000,000 | 260 | 0.76× | 198 | 0.71× | 24 | 21% | 366 MB | 165.7 | 248 MB | 226 MB | 0.69× |
| the five 2k samples | 2,000 each | 86 to 144 | 0.82× to 1.10× | 76 to 158 | 0.82× to 1.04× | 1 or 2 | 1% | | 40 to 90 | 37 to 39 MB | 23 to 24 MB | 0.91× to 1.10× |

Over 90 days:

| Corpus | Disk / raw | Day indexes | Segments | RSS (footprint) after start | after 7 days | after 30 days | after 90 days | Retention to 30 days |
|---|---|---|---|---|---|---|---|---|
| hdfs-2m | 0.68× | 90 | 194 | 19 MB (6.7 MB) | 24 MB (8.1 MB) | 33 MB (8.7 MB) | 52 MB (10.1 MB) | 90 to 30 days, 122 MB freed in 0.52 s |
| synthetic-2m | 0.97× | 90 | 294 | 19 MB (6.4 MB) | 25 MB (7.3 MB) | 38 MB (8.3 MB) | 80 MB (10.5 MB) | 90 to 30 days, 335 MB freed in 0.86 s |

- **Real logs take 0.56× to 1.00× of their raw size, 86 bytes per line at the median, in this slow run (measured).** The seven runs of 25k lines or more give 0.76× at the median. Logs with long repeated text compress best: nginx and HDFS at 0.56× and 0.67×, Linux syslog at 1.00× in its 25k lines. The synthetic pino rows take 0.76×, in line with the real logs. The previous run, at normal priority, gave 0.52× to 0.88× with a median of 0.65× and 74 bytes per line.
- **IP scrubbing makes nginx and OpenSSH smaller.** With IP addresses kept, nginx takes 0.62× instead of 0.56× (one segment: 0.60× instead of 0.54×), because `[Filtered: ip]` repeats where the addresses did not. OpenSSH, with fewer addresses per line, takes 0.70× instead of 0.67× in one segment. Its Disk / raw came out lower with IPs kept, 0.72× against 0.74×, only because that run's segments merged differently. A forecast for a project that turns IP scrubbing off should use the `IP kept` ratios for logs like these.
- **A small corpus costs more per line.** The 2k samples take 0.82× to 1.10×, because a day index has a fixed cost and the docstore compresses better in larger blocks. The forecast should use the large corpora.
- **How close Davy's merges get to the floor depends on how fast it ingested.** A forced merge into one segment saved 5% to 13% here (hdfs-2m 0.67× to 0.58×) against at most 3% in the previous run, and made nginx-parsed 6% larger. The forecast can use Disk / raw as is, since it is what Davy leaves on disk.
- **Parsed attributes cost about a third more.** The same nginx log takes 0.56× as one message and 0.76× split into attributes, because each attribute value is indexed twice, as a JSON path in `attributes` and as a bare word in `values` (24% of the index). The synthetic rows, all attributes, give `values` 21%. A raw line has no attributes, so `values` stays at about 1%, its cost per document.
- **Spreading the same lines over 90 days costs 0.03 to 1.2 MB per extra day index (measured, two corpora).** hdfs-2m went from 0.67× in one day to 0.68× in 90, synthetic-2m from 0.76× to 0.97×. The cost follows the segments a day keeps, so it varies with the ingest rate too: the previous run measured 0.1 to 0.6 MB.
- **Disk needs headroom for merges.** While ingesting, the log directory rose at least 0.10× to 0.40× above its final size (median 0.14×; the previous run saw up to 0.52×). A forced merge into one segment needs most of a copy of the day (0.63× to 0.92×). Davy's own merges rewrite only some segments at a time, so 1× of the largest day index is a safe reserve (inferred).
- **Search memory barely grows with the days opened.** The footprint went from 6.4 to 6.7 MB at start to 10.1 to 10.5 MB after opening 90 days, about 0.04 MB per open day. RSS grew from 19 MB to 52 and 80 MB, but that growth is index pages the queries read, which the OS can drop. These are two light queries per window; a query that reads many documents of every day would touch more pages. Searches were slow at background priority: the first search with no hit over 90 days took 533 to 537 ms, against 23 to 36 ms in the previous run (noisy).
- **Ingest memory is mostly memory the server owns.** Peak footprint was 128 to 226 MB for corpora of 655k lines or more, highest for the 260-byte synthetic rows, against 23 to 24 MB for 2k lines; peak RSS was 15 to 30 MB above it. What that memory holds (writer buffers, the ingest queue, merges) is not measured (noisy).
- **Ingest cost 22 to 166 µs of CPU per row in this run, 6.5 to 27.6 µs in the previous one (noisy).** A raw line cost 22 to 57 µs, a parsed nginx line 51 µs and a synthetic JSON row 166 µs. The previous run is closer to what a dedicated core costs.
- **Retention is cheap.** Removing 60 day indexes took 0.5 to 0.9 s here (0.05 to 0.08 s in the previous run) and freed 122 and 335 MB, because a day past retention is renamed into `.trash` and removed whole. The data ends the day before the run, so 30 day indexes remain.

### Errors

| Issues × events | SQLite B/event | Row B/event | Compacted row B/event | Payloads/issue | B/payload | B/hourly row | B/tag row | B/issue | CPU µs/event |
|---|---|---|---|---|---|---|---|---|---|
| 5,000 × 20 | 2218 | 563 | 553 | 20 | 1572 | 38 | 67 | 599 | 1248 |
| 10 × 10,000 | 2108 | 2082 | 205 | 150 | 1570 | 46 | | | 1152 |

Row is the bytes of an event in the `events` table and its indexes, without its payload. A payload is the zstd event JSON, 0.57× of the 2747 bytes the SDK sent. Each event of the first run fell in an hour of its own, so each made an hourly row. The issue and tag bytes come from the 5,000-issue run only, since 10 issues fit in a few 4 KB pages. Every figure here is for this one event shape; a larger event has a larger payload and scales the payload bytes with it.

- **An event that keeps its payload costs its payload plus about 560 bytes (measured).** The cold-issue run kept every payload, as each issue had only 20 events.
- **A hot issue keeps 150 payloads in 10,000 events, as designed, but as written its events take almost as much disk as if it kept them all.** Without its payload an event takes 205 bytes once compacted, yet 2,082 bytes as written, and 92% of the `events` table was free space inside its pages. The cause: in each write transaction the newest event of an issue keeps its payload, which an `UPDATE` drops once a newer event of the issue arrives in a later transaction. By then the row sits on a page SQLite filled and left, so the space stays empty. The whole database was 211 MB as written and 23 MB after `VACUUM`. So the 2,082 bytes is measured for a 1,570-byte payload and for 10 hot issues sent from 4 connections: it grows with the payload, and with how few events of one issue share a transaction, and it is not a fixed cost per event. This benchmark found it, and this change does not fix it. Until a fix, the forecast should count an event of a hot issue like one that keeps its payload, its compressed payload plus about 560 bytes: this run's 2,082 bytes is 512 above the payload, close to that worst case of one dropped payload per event (inferred).
- **Error ingest cost about 1.2 ms of CPU per event here, 0.3 ms in the previous run (noisy).** The client sent one event per envelope, so this includes HTTP and envelope parsing for every event. Peak footprint was 12 MB, peak RSS 25 MB.

## Rerun

```sh
cd bench/storage
cargo build --release
./run.sh > results.txt 2>&1      # about 45 minutes; data goes to ../../.scratch/bench-data
cd ../..
bash bench/load-logs.sh 1000000 1000 4   # logs ingest through the server, about 1 minute
bash bench/agent-search.sh               # agent search over 90 days, about 5 minutes and 4 GB of scratch
bench/footprint.sh                       # footprint coefficients, macOS only, at normal priority; 1.3 GB of downloads and corpora
                                         # `quick` for the 2k samples only, written to the work directory
                                         # FOOTPRINT_WORK, FOOTPRINT_OUT and FOOTPRINT_SHARED: see the script's header
```
