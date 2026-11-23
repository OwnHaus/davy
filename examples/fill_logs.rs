//! Writes synthetic pino-shaped logs straight into a data directory's log
//! store, spread evenly over the last `span` hours, for timing searches on a
//! large store without timing ingest.
//!
//! cargo run --release --example fill_logs -- DATA ROWS HOURS PROJECTS

use anyhow::{Context, Result, bail};
use davy::logs::{
    LogLevel, LogRecord, LogSource,
    store::{LogWriter, QueueError},
};
use serde_json::{Map, Value, json};
use std::{
    io::Write,
    time::{Duration, Instant},
};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        let i = self.next() % items.len() as u64;
        items[usize::try_from(i).expect("an index below a slice length fits usize")]
    }
    fn hex(&mut self, len: usize) -> String {
        (0..len)
            .map(|_| char::from(self.pick(b"0123456789abcdef")))
            .collect()
    }
}

const ROUTES: [&str; 6] = [
    "/v1/events",
    "/v1/orders",
    "/v1/cart",
    "/health",
    "/v1/users/me",
    "/v1/search",
];
const SERVICES: [&str; 4] = ["api", "worker", "checkout", "search"];

fn object(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

/// The same mix as `load_logs`: 70% requests, 20% info, 7% warn, 3% error.
fn record(rng: &mut Rng, project_id: i64, timestamp: i64) -> LogRecord {
    let mut attributes = object(json!({
        "pid": 4000 + rng.below(8),
        "hostname": format!("api-7d9f-{}", rng.hex(4)),
    }));
    let roll = rng.below(100);
    let (level, message, extra, trace_id) = if roll < 70 {
        let route = rng.pick(&ROUTES);
        let status = rng.pick(&[200, 200, 200, 201, 204, 304, 404, 500]);
        let method = rng.pick(&["GET", "POST"]);
        let extra = json!({
            "req": {"id": format!("r-{}", rng.hex(8)), "method": method, "url": format!("{route}/{}", rng.below(100_000))},
            "res": {"statusCode": status},
            "responseTime": rng.below(900),
        });
        let message = "request completed".to_owned();
        (LogLevel::Info, message, extra, Some(rng.hex(32)))
    } else if roll < 90 {
        let message = format!("cache refreshed for tenant t-{}", rng.below(500));
        let extra = json!({"user": format!("u-{}", rng.below(50_000))});
        (LogLevel::Info, message, extra, None)
    } else if roll < 97 {
        let ms = 1000 + rng.below(9000);
        let message = format!("slow query on orders took {ms}ms");
        (LogLevel::Warn, message, json!({"ms": ms}), None)
    } else {
        let extra = json!({"err": {
            "type": "Error",
            "message": "connect ETIMEDOUT 10.0.1.5:5432",
            "stack": format!("Error: connect ETIMEDOUT 10.0.1.5:5432\n    at TCPConnectWrap.afterConnect (node:net:1611:16)\n    at Pool.connect (/app/node_modules/pg-pool/index.js:{}:9)", rng.below(400)),
        }});
        (LogLevel::Error, "database timeout".to_owned(), extra, None)
    };
    attributes.extend(object(extra));
    LogRecord {
        project_id,
        timestamp,
        observed_at: timestamp,
        source: LogSource::Logtail,
        level,
        message,
        service: Some(rng.pick(&SERVICES).to_owned()),
        trace_id,
        span_id: None,
        attributes,
    }
}

fn main() -> Result<()> {
    let usage = "usage: fill_logs DATA ROWS HOURS PROJECTS";
    let mut args = std::env::args().skip(1);
    let mut next = || args.next().context(usage);
    let data = std::path::PathBuf::from(next()?);
    let rows: i64 = next()?.parse().context(usage)?;
    let hours: i64 = next()?.parse().context(usage)?;
    let projects: u64 = next()?.parse().context(usage)?;
    let span = hours * 3_600_000_000_000;
    let end = davy::time::now_ns();
    let writer = LogWriter::start(davy::logs::dir(&data), 30)?;
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let started = Instant::now();
    let batch = 5_000;
    for first in (0..rows).step_by(batch) {
        let batch: Vec<_> = (first..(first + i64::try_from(batch)?).min(rows))
            .map(|i| {
                let project = i64::try_from(1 + rng.below(projects)).unwrap_or(1);
                record(&mut rng, project, end - span + span / rows * i)
            })
            .collect();
        loop {
            match writer.queue.push(batch.clone()) {
                Ok(()) => break,
                Err(QueueError::Full) => std::thread::sleep(Duration::from_millis(5)),
                Err(QueueError::TooLarge) => bail!("a batch is larger than the log queue"),
                Err(QueueError::Stopped) => bail!("the log writer stopped"),
            }
        }
    }
    drop(writer.queue);
    writer
        .thread
        .join()
        .map_err(|_panic| anyhow::anyhow!("the log writer panicked"))?;
    writeln!(
        std::io::stdout(),
        "wrote {rows} logs over {hours} h in {:.1}s",
        started.elapsed().as_secs_f64()
    )?;
    Ok(())
}
