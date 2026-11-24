//! Sends synthetic pino-shaped logs to a running `davy serve` over the Logtail wire
//! format (gzip msgpack, batches of `--batch`) and reports the accept rate.
//!
//! cargo run --release --example load_logs -- 127.0.0.1:PORT KEY 1000000 1000 4

use anyhow::{Context, Result};
use serde_json::Value;
use std::{
    io::{Read, Write},
    net::TcpStream,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

#[path = "shared/pino.rs"]
mod pino;
use pino::{Rng, row};

fn msgpack(v: &Value) -> rmpv::Value {
    use rmpv::Value as M;
    match v {
        Value::Null => M::Nil,
        Value::Bool(b) => M::Boolean(*b),
        Value::Number(n) => n
            .as_i64()
            .map_or_else(|| M::F64(n.as_f64().unwrap_or(0.0)), M::from),
        Value::String(s) => M::from(s.as_str()),
        Value::Array(a) => M::Array(a.iter().map(msgpack).collect()),
        Value::Object(o) => M::Map(
            o.iter()
                .map(|(k, v)| (M::from(k.as_str()), msgpack(v)))
                .collect(),
        ),
    }
}

fn post(addr: &str, key: &str, body: &[u8]) -> std::io::Result<u16> {
    let mut s = TcpStream::connect(addr)?;
    write!(
        s,
        "POST / HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {key}\r\nContent-Type: application/msgpack\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    s.write_all(body)?;
    let mut response = String::new();
    s.read_to_string(&mut response)?;
    Ok(response
        .split(' ')
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0))
}

struct Args {
    addr: String,
    key: String,
    total: usize,
    batch: usize,
    workers: usize,
}

fn args() -> Result<Args> {
    let usage = "usage: load_logs ADDR KEY TOTAL BATCH WORKERS";
    let mut args = std::env::args().skip(1);
    let mut next = || args.next().context(usage);
    Ok(Args {
        addr: next()?,
        key: next()?,
        total: next()?.parse().context(usage)?,
        batch: next()?.parse().context(usage)?,
        workers: next()?.parse().context(usage)?,
    })
}

struct Bodies {
    gzipped: Vec<Vec<u8>>,
    raw_ndjson: usize,
    raw_msgpack: usize,
}

fn generate(total: usize, batch: usize) -> Result<Bodies> {
    let total_rows = i64::try_from(total)?;
    let start_ms = jiff::Timestamp::now().as_millisecond() - total_rows / 50;
    let mut rng = Rng::seeded();
    let mut out = Bodies {
        gzipped: Vec::new(),
        raw_ndjson: 0,
        raw_msgpack: 0,
    };
    for b in 0..total.div_ceil(batch) {
        let mut rows = Vec::with_capacity(batch);
        for i in b * batch..((b + 1) * batch).min(total) {
            let r = row(&mut rng, i64::try_from(i)?, start_ms)?;
            out.raw_ndjson += serde_json::to_vec(&r)?.len() + 1;
            let r = msgpack(&r);
            let mut buf = Vec::new();
            rmpv::encode::write_value(&mut buf, &r)?;
            out.raw_msgpack += buf.len();
            rows.push(r);
        }
        let mut packed = Vec::new();
        rmpv::encode::write_value(&mut packed, &rmpv::Value::Array(rows))?;
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&packed)?;
        out.gzipped.push(gz.finish()?);
    }
    Ok(out)
}

struct Outcome {
    throttled: usize,
    failures: Vec<String>,
}

fn send_all(args: &Args, bodies: Vec<Vec<u8>>) -> Result<Outcome> {
    let bodies = Arc::new(bodies);
    let next = Arc::new(AtomicUsize::new(0));
    let throttled = Arc::new(AtomicUsize::new(0));
    let failures = Arc::new(Mutex::new(Vec::new()));
    let threads: Vec<_> = (0..args.workers)
        .map(|_| {
            let (bodies, next, throttled, failures) = (
                bodies.clone(),
                next.clone(),
                throttled.clone(),
                failures.clone(),
            );
            let (addr, key) = (args.addr.clone(), args.key.clone());
            std::thread::spawn(move || {
                while let Some(body) = bodies.get(next.fetch_add(1, Ordering::Relaxed)) {
                    loop {
                        match post(&addr, &key, body) {
                            Ok(202) => break,
                            Ok(429) => {
                                throttled.fetch_add(1, Ordering::Relaxed);
                                std::thread::sleep(Duration::from_millis(50));
                            }
                            other => {
                                failures
                                    .lock()
                                    .expect("no sender panics while holding the lock")
                                    .push(format!("{other:?}"));
                                break;
                            }
                        }
                    }
                }
            })
        })
        .collect();
    for t in threads {
        t.join()
            .map_err(|_panic| anyhow::anyhow!("a sender thread panicked"))?;
    }
    let failures = std::mem::take(
        &mut *failures
            .lock()
            .map_err(|e| anyhow::anyhow!("failure list poisoned: {e}"))?,
    );
    Ok(Outcome {
        throttled: throttled.load(Ordering::Relaxed),
        failures,
    })
}

#[expect(
    clippy::cast_precision_loss,
    reason = "a rate for a report, not an exact count"
)]
fn per_second(rows: usize, secs: f64) -> f64 {
    rows as f64 / secs
}

fn main() -> Result<()> {
    let args = args()?;
    let generating = Instant::now();
    let bodies = generate(args.total, args.batch)?;
    let gzipped: usize = bodies.gzipped.iter().map(Vec::len).sum();
    writeln!(
        std::io::stderr(),
        "generated {} rows in {:.1}s: ndjson {} B ({} B/row), msgpack {} B, gzip {gzipped} B",
        args.total,
        generating.elapsed().as_secs_f64(),
        bodies.raw_ndjson,
        bodies.raw_ndjson / args.total,
        bodies.raw_msgpack,
    )?;

    let sending = Instant::now();
    let outcome = send_all(&args, bodies.gzipped)?;
    let secs = sending.elapsed().as_secs_f64();
    let mut out = std::io::stdout();
    writeln!(
        out,
        "accepted {} rows in {secs:.2}s = {:.0} rows/s, {} responses 429, {} failures {:?}",
        args.total,
        per_second(args.total, secs),
        outcome.throttled,
        outcome.failures.len(),
        outcome.failures.iter().take(3).collect::<Vec<_>>()
    )?;
    writeln!(out, "raw_ndjson_bytes={}", bodies.raw_ndjson)?;
    Ok(())
}
