//! Measures ingest while a day turns cold, see "Cold tier" in
//! docs/design.md and docs/storage-benchmark.md.
//!
//! cargo run --release --example cold_ingest -- WORK [ROWS]
//!
//! Writes ROWS logs (1 000 000 by default) into a day ten days back, then
//! starts a log writer that keeps 7 days hot, so that day turns cold, and
//! ingests logs for today until it is cold. Then ingests as long again
//! with a writer that keeps every day hot. WORK is an empty directory.
//! Prints rows a second of each run, and how long the conversion took.

use anyhow::{Result, bail};
use davy::{
    logs::{
        LogLevel, LogRecord, LogSource,
        store::{LogWriter, QueueError, cold::DIR, no_overrides},
    },
    time::{DAY_NS, day_of, midnight, now_ns},
};
use serde_json::{Map, Value, json};
use std::{
    io::Write,
    path::Path,
    time::{Duration, Instant},
};

const BATCH: usize = 5_000;

/// A log of a web service, the `i`th of its day.
fn record(i: usize, ts: i64) -> LogRecord {
    let status = [200, 200, 200, 201, 404, 500][i % 6];
    let attributes = json!({
        "route": format!("/api/orders/{}", i % 977),
        "status": status,
        "user_id": format!("u{}", i % 10_007),
        "took_ms": i % 1_500,
    });
    LogRecord {
        project_id: 1 + i64::try_from(i % 3).unwrap_or(0),
        timestamp: ts,
        observed_at: ts,
        source: LogSource::Otlp,
        level: if status == 500 {
            LogLevel::Error
        } else {
            LogLevel::Info
        },
        level_unknown: false,
        message: format!("GET /api/orders/{} {status} in {} ms", i % 977, i % 1_500),
        service: Some("shop".to_owned()),
        trace_id: None,
        span_id: None,
        attributes: match attributes {
            Value::Object(m) => m,
            _ => Map::new(),
        },
    }
}

/// Queues `batch`, waiting while the queue is full.
fn push(writer: &LogWriter, batch: Vec<LogRecord>) -> Result<()> {
    while !writer.queue.has_room(&batch) {
        std::thread::sleep(Duration::from_millis(1));
    }
    match writer.queue.push(batch) {
        Ok(()) => Ok(()),
        Err(QueueError::Full | QueueError::TooLarge | QueueError::Stopped) => {
            bail!("the log writer refused a batch")
        }
    }
}

fn stop(writer: LogWriter) -> Result<()> {
    drop(writer.queue);
    writer
        .thread
        .join()
        .map_err(|_panic| anyhow::anyhow!("the log writer panicked"))
}

/// Ingests logs for today with `writer` until `done`, at most `limit`.
/// Returns the rows queued and the time taken.
fn ingest(writer: &LogWriter, limit: Duration, done: impl Fn() -> bool) -> Result<(usize, f64)> {
    let start = Instant::now();
    let mut rows = 0;
    while !done() && start.elapsed() < limit {
        let now = now_ns();
        let batch = (0..BATCH).map(|j| record(rows + j, now)).collect();
        push(writer, batch)?;
        rows += BATCH;
    }
    Ok((rows, start.elapsed().as_secs_f64()))
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let Some(work) = args.next() else {
        bail!("usage: cold_ingest WORK [ROWS]");
    };
    let total: usize = args.next().map_or(Ok(1_000_000), |n| n.parse())?;
    let root = davy::logs::dir(Path::new(&work));
    let old = day_of(now_ns() - 10 * DAY_NS);
    let step = DAY_NS / i64::try_from(total)?;

    let t = Instant::now();
    let writer = LogWriter::start(root.clone(), 30, no_overrides())?;
    for b in 0..total.div_ceil(BATCH) {
        let batch = (b * BATCH..((b + 1) * BATCH).min(total))
            .map(|i| record(i, midnight(old) + step * i64::try_from(i).unwrap_or(0)))
            .collect();
        push(&writer, batch)?;
    }
    stop(writer)?;
    let mut out = std::io::stdout();
    writeln!(
        out,
        "wrote {total} rows for {old} in {:.1} s",
        t.elapsed().as_secs_f64()
    )?;

    let hot = root.join(old.to_string());
    let segment = root.join(DIR).join(old.to_string()).join("1");
    let writer = LogWriter::start_with(root.clone(), 30, no_overrides(), Some(7))?;
    let cold = || !hot.exists() && segment.exists();
    let (rows, took) = ingest(&writer, Duration::from_secs(600), cold)?;
    stop(writer)?;
    if !cold() {
        bail!("{old} is not cold after {took:.0} s");
    }
    let during = f64::from(u32::try_from(rows)?) / took;
    writeln!(
        out,
        "during conversion\t{rows} rows in {took:.1} s\t{during:.0} rows/s"
    )?;

    let writer = LogWriter::start(root, 30, no_overrides())?;
    let (rows, took) = ingest(&writer, Duration::from_secs_f64(took), || false)?;
    stop(writer)?;
    let alone = f64::from(u32::try_from(rows)?) / took;
    writeln!(
        out,
        "without conversion\t{rows} rows in {took:.1} s\t{alone:.0} rows/s"
    )?;
    writeln!(out, "ratio\t{:.2}", during / alone)?;
    Ok(())
}
