//! Sends synthetic error events to a running `davy serve` over the Sentry
//! store route and reports the accept rate. A `200` means the writer
//! committed the event, so the rate is what the store sustains. With
//! `once` each event is sent once whatever the answer, which measures
//! what a refusal costs against a project past its quota.
//!
//! cargo run --release --example load_events -- 127.0.0.1:PORT PROJECT KEY 50000 8 [once]

use anyhow::{Context, Result};
use std::{
    io::{Read, Write},
    net::TcpStream,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

/// Distinct issues the events fall into, as a busy release might have.
const ISSUES: usize = 50;

struct Args {
    addr: String,
    project: String,
    key: String,
    total: usize,
    workers: usize,
    once: bool,
}

fn args() -> Result<Args> {
    let usage = "usage: load_events ADDR PROJECT KEY TOTAL WORKERS [once]";
    let mut args = std::env::args().skip(1);
    let mut next = || args.next().context(usage);
    Ok(Args {
        addr: next()?,
        project: next()?,
        key: next()?,
        total: next()?.parse().context(usage)?,
        workers: next()?.parse().context(usage)?,
        once: args.next().as_deref() == Some("once"),
    })
}

/// An event of about 1.5 KB with a stack trace of eight frames, in one of
/// [`ISSUES`] groups.
fn event(i: usize) -> String {
    let issue = i % ISSUES;
    let frames: Vec<String> = (0..8)
        .map(|f| {
            format!(
                r#"{{"filename":"src/app/module{issue}/file{f}.js","function":"handler{issue}_{f}","lineno":{},"colno":17,"in_app":true}}"#,
                10 + f * 7
            )
        })
        .collect();
    format!(
        r#"{{"event_id":"{i:032x}","platform":"node","level":"error","release":"web@1.0.{}","environment":"production","message":"request {i} failed","tags":{{"route":"/orders/{issue}"}},"exception":{{"values":[{{"type":"TypeError","value":"Cannot read properties of undefined (reading 'f{issue}')","stacktrace":{{"frames":[{}]}}}}]}}}}"#,
        i % 3,
        frames.join(",")
    )
}

fn post(addr: &str, path: &str, body: &[u8]) -> std::io::Result<u16> {
    let mut s = TcpStream::connect(addr)?;
    write!(
        s,
        "POST {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
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

/// Sends every event, each until it is taken or with `once` one time, and
/// returns the `429`s and the answers other than `200`.
fn send_all(args: &Args) -> Result<(usize, usize)> {
    let next = Arc::new(AtomicUsize::new(0));
    let throttled = Arc::new(AtomicUsize::new(0));
    let failed = Arc::new(AtomicUsize::new(0));
    let path = format!("/api/{}/store/?sentry_key={}", args.project, args.key);
    let threads: Vec<_> = (0..args.workers)
        .map(|_| {
            let (next, throttled, failed) = (next.clone(), throttled.clone(), failed.clone());
            let (addr, path, total, once) =
                (args.addr.clone(), path.clone(), args.total, args.once);
            std::thread::spawn(move || {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= total {
                        break;
                    }
                    let body = event(i);
                    loop {
                        match post(&addr, &path, body.as_bytes()) {
                            Ok(200) => break,
                            Ok(429) => {
                                throttled.fetch_add(1, Ordering::Relaxed);
                                if once {
                                    break;
                                }
                                std::thread::sleep(Duration::from_millis(50));
                            }
                            _ => {
                                failed.fetch_add(1, Ordering::Relaxed);
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
    Ok((
        throttled.load(Ordering::Relaxed),
        failed.load(Ordering::Relaxed),
    ))
}

#[expect(
    clippy::cast_precision_loss,
    reason = "a rate for a report, not an exact count"
)]
fn per_second(n: usize, secs: f64) -> f64 {
    n as f64 / secs
}

fn main() -> Result<()> {
    let args = args()?;
    let bytes: usize = (0..ISSUES).map(|i| event(i).len()).sum::<usize>() / ISSUES;
    let sending = Instant::now();
    let (throttled, failed) = send_all(&args)?;
    let secs = sending.elapsed().as_secs_f64();
    let report = if args.once {
        format!(
            "sent {} events of about {bytes} B once in {secs:.2}s = {:.0} requests/s, {throttled} responses 429, {failed} other",
            args.total,
            per_second(args.total, secs),
        )
    } else {
        format!(
            "accepted {} events of about {bytes} B in {secs:.2}s = {:.0} events/s, {throttled} responses 429, {failed} failures",
            args.total,
            per_second(args.total, secs),
        )
    };
    writeln!(std::io::stdout(), "{report}")?;
    Ok(())
}
