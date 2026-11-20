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

## Rerun

```sh
cd bench/storage
cargo build --release
./run.sh > results.txt 2>&1      # about 45 minutes; data goes to ../../.scratch/bench-data
```
