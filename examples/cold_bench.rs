//! Measures the cold tier on a corpus: the corpus goes through the log
//! writer into one day index, which is then converted with each zstd
//! level, block size and dictionary choice, and each result is scanned.
//! See "Cold tier" in docs/storage-benchmark.md.
//!
//! cargo run --release --example cold_bench -- CORPUS SHAPE WORK [QUERY]...
//!
//! SHAPE is `line` (each line a message), `json` (a pino row) or `nginx`
//! (an access log line split into attributes). WORK is an empty directory
//! for the data. Prints one tab-separated line per setting.

use anyhow::{Context, Result, bail};
use davy::logs::{
    LogLevel, LogRecord, LogSource,
    store::{LogWriter, QueueError, cold::bench},
};
use regex::Regex;
use serde_json::{Map, Value, json};
use std::{
    io::Write,
    os::unix::fs::MetadataExt,
    path::Path,
    time::{Duration, Instant},
};

const BATCH: usize = 5_000;
const LEVELS: &str = "3,9,19";
const BLOCKS: &str = "256,1024,4096";

/// The numbers in the variable `name`, `default` when it is not set.
fn list(name: &str, default: &str) -> Result<Vec<usize>> {
    std::env::var(name)
        .unwrap_or_else(|_| default.to_owned())
        .split(',')
        .map(|n| n.trim().parse().with_context(|| format!("{name}: {n}")))
        .collect()
}

fn out(line: &str) -> Result<()> {
    writeln!(std::io::stdout(), "{line}")?;
    Ok(())
}

fn nginx(line: &str, re: &Regex) -> Map<String, Value> {
    let Some(c) = re.captures(line) else {
        return Map::new();
    };
    let number = |i: usize| c[i].parse::<u64>().map_or(Value::Null, Value::from);
    let v = json!({
        "remote_addr": &c[1],
        "remote_user": &c[3],
        "time_local": &c[4],
        "status": number(6),
        "body_bytes_sent": number(7),
        "http_referer": &c[8],
        "http_user_agent": &c[9],
    });
    match v {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

/// The record a log shipper would send for `line` at `ts`.
fn record(line: &str, shape: &str, re: &Regex, ts: i64) -> Result<LogRecord> {
    let mut r = LogRecord {
        project_id: 1,
        timestamp: ts,
        observed_at: ts,
        source: LogSource::Logtail,
        level: LogLevel::Info,
        level_unknown: false,
        message: line.to_owned(),
        service: None,
        trace_id: None,
        span_id: None,
        attributes: Map::new(),
    };
    match shape {
        "line" => {}
        "nginx" => {
            r.attributes = nginx(line, re);
            if let Some(c) = re.captures(line) {
                c[5].clone_into(&mut r.message);
            }
        }
        "json" => {
            let Value::Object(mut m) = serde_json::from_str(line).context("a json line")? else {
                bail!("a json line is an object");
            };
            m.remove("dt");
            let text = |m: &mut Map<String, Value>, k: &str| match m.remove(k) {
                Some(Value::String(s)) => Some(s),
                _ => None,
            };
            r.message = text(&mut m, "message").unwrap_or_default();
            r.service = text(&mut m, "service");
            r.trace_id = text(&mut m, "trace_id");
            r.level = match text(&mut m, "level").as_deref() {
                Some("warn") => LogLevel::Warn,
                Some("error") => LogLevel::Error,
                Some("debug") => LogLevel::Debug,
                _ => LogLevel::Info,
            };
            r.attributes = m;
        }
        other => bail!("unknown shape {other}"),
    }
    Ok(r)
}

/// Writes the corpus into one day index two days back. Returns the day,
/// the lines and their raw bytes.
fn ingest(corpus: &Path, shape: &str, data: &Path) -> Result<(jiff::civil::Date, usize, usize)> {
    let bytes = std::fs::read(corpus)?;
    let lines: Vec<String> = bytes
        .split(|b| *b == b'\n')
        .filter(|l| !l.trim_ascii().is_empty())
        .map(|l| String::from_utf8_lossy(l.strip_suffix(b"\r").unwrap_or(l)).into_owned())
        .collect();
    let raw = bytes.len();
    let day = davy::time::day_of(davy::time::now_ns()) - jiff::ToSpan::days(2);
    let start = davy::time::midnight(day);
    let step = davy::time::DAY_NS / i64::try_from(lines.len().max(1))?;
    let re = Regex::new(
        r#"^(\S+) (\S+) (\S+) \[([^\]]+)\] "([^"]*)" (\d{3}) (\S+) "([^"]*)" "([^"]*)""#,
    )?;
    let writer = LogWriter::start(
        davy::logs::dir(data),
        36_500,
        davy::logs::store::no_overrides(),
    )?;
    for (b, chunk) in lines.chunks(BATCH).enumerate() {
        let batch = chunk
            .iter()
            .enumerate()
            .map(|(j, l)| record(l, shape, &re, start + step * i64::try_from(b * BATCH + j)?))
            .collect::<Result<Vec<_>>>()?;
        while !writer.queue.has_room(&batch) {
            std::thread::sleep(Duration::from_millis(5));
        }
        match writer.queue.push(batch) {
            Ok(()) => {}
            Err(QueueError::Full | QueueError::TooLarge | QueueError::Stopped) => {
                bail!("the log writer refused a batch")
            }
        }
    }
    drop(writer.queue);
    writer
        .thread
        .join()
        .map_err(|_panic| anyhow::anyhow!("the log writer panicked"))?;
    Ok((day, lines.len(), raw))
}

/// The bytes the files in `dir` take on disk.
fn disk(dir: &Path) -> Result<u64> {
    let mut n = 0;
    for e in std::fs::read_dir(dir)? {
        let m = e?.metadata()?;
        if m.is_file() {
            n += m.blocks() * 512;
        }
    }
    Ok(n)
}

#[expect(clippy::cast_precision_loss, reason = "ratios to 3 digits")]
fn main() -> Result<()> {
    let usage = "usage: cold_bench CORPUS SHAPE WORK [QUERY]...";
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [corpus, shape, work, queries @ ..] = args.as_slice() else {
        bail!(usage);
    };
    let work = Path::new(work);
    let data = work.join("data");
    let (day, lines, raw) = ingest(Path::new(corpus), shape, &data)?;
    let hot = davy::logs::dir(&data).join(day.to_string());
    let hot_disk = disk(&hot)?;
    out(&format!(
        "corpus\t{corpus}\tlines\t{lines}\traw\t{raw}\thot\t{hot_disk}\thot/raw\t{:.3}",
        hot_disk as f64 / raw as f64
    ))?;
    let levels = list("COLD_LEVELS", LEVELS)?;
    let blocks = list("COLD_BLOCKS", BLOCKS)?;
    let dicts: Vec<bool> = list("COLD_DICTS", "0,1")?
        .into_iter()
        .map(|d| d == 1)
        .collect();
    // Text queries cost the same at every level and block size, so a long
    // corpus runs them only at the settings in COLD_QUERY_AT, as
    // `level:kib:dict` separated by commas. All when it is not set.
    let query_at = std::env::var("COLD_QUERY_AT").ok();
    out(
        "level\tblock\tdict\tfile_bytes\tdisk\tdisk/raw\tcold/hot\tblocks\tconvert_s\tconvert_rows/s\tquery\tmatched\tscan_ms\tscan_rows/s\tscan_MB/s",
    )?;
    for &dict in &dicts {
        for &level in &levels {
            for &kib in &blocks {
                let (level, block) = (i32::try_from(level)?, kib << 10);
                let dir = work.join(format!("cold-{level}-{block}-{dict}"));
                std::fs::create_dir_all(&dir)?;
                let t = Instant::now();
                let w = bench::convert(&hot, day, &dir, level, block, dict)?;
                let took = t.elapsed().as_secs_f64();
                let bytes = disk(&dir)?;
                let head = format!(
                    "{level}\t{}\t{dict}\t{}\t{bytes}\t{:.3}\t{:.3}\t{}\t{took:.2}\t{:.0}",
                    block >> 10,
                    w.bytes,
                    bytes as f64 / raw as f64,
                    bytes as f64 / hot_disk as f64,
                    w.blocks,
                    w.rows as f64 / took,
                );
                let mut all = vec![None];
                let here = format!("{level}:{kib}:{}", u8::from(dict));
                if query_at
                    .as_deref()
                    .is_none_or(|at| at.split(',').any(|s| s == here))
                {
                    all.extend(queries.iter().map(|q| Some(q.as_str())));
                }
                for q in all {
                    let t = Instant::now();
                    let (found, rows) = bench::scan(&w.files, q)?;
                    let s = t.elapsed().as_secs_f64();
                    out(&format!(
                        "{head}\t{}\t{found}\t{:.0}\t{:.0}\t{:.0}",
                        q.unwrap_or("(all)"),
                        s * 1e3,
                        rows as f64 / s,
                        w.raw as f64 / s / 1e6,
                    ))?;
                }
                std::fs::remove_dir_all(&dir)?;
            }
        }
    }
    Ok(())
}
