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

## Rerun

```sh
cd bench/storage
cargo build --release
./run.sh > results.txt 2>&1      # about 45 minutes; data goes to ../../.scratch/bench-data
cd ../..
bash bench/load-logs.sh 1000000 1000 4   # logs ingest through the server, about 1 minute
```
