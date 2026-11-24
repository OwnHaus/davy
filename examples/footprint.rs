//! Measures what a corpus of logs or errors costs Davy in disk, memory and
//! CPU. `bench/footprint.sh` drives it and reads its `key=value` lines.
//!
//! footprint synth ROWS                   synthetic pino NDJSON on stdout
//! footprint logs ADDR KEY FILE SHAPE DAYS
//!     sends each line of FILE as a log record, spread evenly over the DAYS
//!     whole UTC days before today. SHAPE is `line` (the line is the
//!     message), `json` (the line is a JSON record) or `nginx` (a combined
//!     access log line, parsed into attributes).
//! footprint errors ADDR KEY PROJECT ENVELOPE ISSUES PER_ISSUE DAYS
//!     sends ISSUES * PER_ISSUE copies of the event in a captured envelope,
//!     each with its own id, fingerprint and time in the DAYS whole UTC days
//!     before today.
//! footprint inspect DATA                 bytes per field of the log store
//! footprint merge DATA                   merges each day into one segment
//! footprint retain DATA DAYS             one retention pass that keeps DAYS

use anyhow::{Context, Result, anyhow, bail};
use regex::Regex;
use serde_json::{Map, Value, json};
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};
use tantivy::{Index, IndexWriter, ReloadPolicy, TantivyDocument, space_usage::PerFieldSpaceUsage};

#[path = "shared/pino.rs"]
mod pino;

const DAY_MS: i64 = 86_400_000;
const BATCH: usize = 1000;
const CONNECTIONS: usize = 4;

fn out(line: &str) -> Result<()> {
    writeln!(std::io::stdout(), "{line}")?;
    Ok(())
}

#[expect(
    clippy::cast_precision_loss,
    reason = "a rate for a report, not an exact count"
)]
fn ratio(a: usize, b: f64) -> f64 {
    a as f64 / b
}

/// The `days` whole UTC days before today, in milliseconds, end exclusive.
/// Whole days make the spacing of the timestamps, and so the bytes of the
/// `ts` field, the same at any time of day.
fn span(days: i64) -> (i64, i64) {
    let now = davy::time::now_ms();
    let midnight = now - now.rem_euclid(DAY_MS);
    (midnight - days * DAY_MS, midnight)
}

/// The time of item `i` of `n`, spread evenly over `[start, end)`.
fn at(start: i64, end: i64, i: usize, n: usize) -> Result<i64> {
    let offset = i128::from(end - start) * i128::try_from(i)? / i128::try_from(n.max(1))?;
    Ok(start + i64::try_from(offset)?)
}

fn rfc3339(ms: i64) -> Result<String> {
    Ok(format!(
        "{:.3}",
        jiff::Timestamp::from_millisecond(ms).context("timestamp out of range")?
    ))
}

/// One keep-alive HTTP/1.1 connection, opened again after an error.
struct Conn {
    addr: String,
    stream: Option<BufReader<TcpStream>>,
}

impl Conn {
    fn new(addr: &str) -> Self {
        Self {
            addr: addr.to_owned(),
            stream: None,
        }
    }

    fn post(&mut self, path: &str, headers: &str, body: &[u8]) -> Result<u16> {
        let r = self.try_post(path, headers, body);
        if r.is_err() {
            self.stream = None;
        }
        r
    }

    fn try_post(&mut self, path: &str, headers: &str, body: &[u8]) -> Result<u16> {
        if self.stream.is_none() {
            self.stream = Some(BufReader::new(TcpStream::connect(&self.addr)?));
        }
        let s = self.stream.as_mut().context("connected above")?;
        let head = format!(
            "POST {path} HTTP/1.1\r\nHost: {}\r\n{headers}Content-Length: {}\r\n\r\n",
            self.addr,
            body.len()
        );
        s.get_mut().write_all(head.as_bytes())?;
        s.get_mut().write_all(body)?;
        let mut line = String::new();
        s.read_line(&mut line)?;
        let status = line
            .split(' ')
            .nth(1)
            .and_then(|c| c.parse().ok())
            .with_context(|| format!("bad status line {line:?}"))?;
        let (mut length, mut close) = (None, false);
        loop {
            line.clear();
            if s.read_line(&mut line)? == 0 {
                bail!("connection closed in the response headers");
            }
            let header = line.trim_end().to_ascii_lowercase();
            if header.is_empty() {
                break;
            }
            if let Some(v) = header.strip_prefix("content-length:") {
                length = Some(v.trim().parse::<usize>()?);
            }
            close |= header == "connection: close";
        }
        let length = length.context("a response without content-length")?;
        let mut sink = vec![0; length];
        s.read_exact(&mut sink)?;
        if close {
            self.stream = None;
        }
        Ok(status)
    }
}

/// Posts every body from `CONNECTIONS` threads, retrying a `429` after
/// 50 ms. Returns the number of `429`s.
fn send_all(
    addr: &str,
    path: &str,
    headers: &str,
    count: usize,
    body: impl Fn(usize) -> Result<Vec<u8>> + Send + Sync,
) -> Result<usize> {
    let next = AtomicUsize::new(0);
    let throttled = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..CONNECTIONS)
            .map(|_| {
                scope.spawn(|| -> Result<()> {
                    let mut conn = Conn::new(addr);
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        if i >= count {
                            return Ok(());
                        }
                        let body = body(i)?;
                        let mut errors = 0;
                        loop {
                            match conn.post(path, headers, &body) {
                                Ok(200 | 202) => break,
                                Ok(429) => {
                                    throttled.fetch_add(1, Ordering::Relaxed);
                                    std::thread::sleep(Duration::from_millis(50));
                                }
                                Ok(status) => bail!("item {i}: HTTP {status}"),
                                Err(e) if errors < 3 => {
                                    errors += 1;
                                    writeln!(std::io::stderr(), "item {i}: {e:#}, retrying")?;
                                }
                                Err(e) => return Err(e),
                            }
                        }
                    }
                })
            })
            .collect();
        for w in workers {
            w.join().map_err(|_panic| anyhow!("a sender panicked"))??;
        }
        Ok(throttled.load(Ordering::Relaxed))
    })
}

fn nginx_regex() -> Result<Regex> {
    Ok(Regex::new(
        r#"^(\S+) (\S+) (\S+) \[([^\]]+)\] "([^"]*)" (\d{3}) (\S+) "([^"]*)" "([^"]*)""#,
    )?)
}

/// The record a log shipper would send for `line`, without its time.
fn shaped(line: &str, shape: &str, nginx: &Regex) -> Result<Map<String, Value>> {
    let v = match shape {
        "line" => json!({ "message": line }),
        "json" => serde_json::from_str(line).context("a line of a json corpus")?,
        "nginx" => match nginx.captures(line) {
            Some(c) => {
                let number = |i: usize| c[i].parse::<u64>().map_or(Value::Null, Value::from);
                json!({
                    "message": &c[5],
                    "remote_addr": &c[1],
                    "remote_user": &c[3],
                    "time_local": &c[4],
                    "status": number(6),
                    "body_bytes_sent": number(7),
                    "http_referer": &c[8],
                    "http_user_agent": &c[9],
                })
            }
            None => json!({ "message": line }),
        },
        other => bail!("unknown shape {other}"),
    };
    match v {
        Value::Object(m) => Ok(m),
        _ => bail!("a record is a JSON object"),
    }
}

/// Gzipped NDJSON bodies of `BATCH` records each, and the raw bytes of the
/// lines they came from. Real logs hold bytes that are not UTF-8, which
/// become U+FFFD, as a log shipper would send them.
fn log_bodies(file: &Path, shape: &str, days: i64) -> Result<(Vec<Vec<u8>>, usize, usize)> {
    let bytes = std::fs::read(file).with_context(|| format!("reading {}", file.display()))?;
    let kept: Vec<&[u8]> = bytes
        .split(|b| *b == b'\n')
        .filter(|l| !l.trim_ascii().is_empty())
        .collect();
    let unterminated = bytes
        .rsplit(|b| *b == b'\n')
        .next()
        .is_some_and(|l| !l.trim_ascii().is_empty());
    let raw = kept.iter().map(|l| l.len() + 1).sum::<usize>() - usize::from(unterminated);
    let lines: Vec<std::borrow::Cow<'_, str>> = kept
        .iter()
        .map(|l| String::from_utf8_lossy(l.strip_suffix(b"\r").unwrap_or(l)))
        .collect();
    let (start, end) = span(days);
    let nginx = nginx_regex()?;
    let mut bodies = Vec::new();
    for (b, chunk) in lines.chunks(BATCH).enumerate() {
        let mut ndjson = Vec::new();
        for (j, line) in chunk.iter().enumerate() {
            let mut m = shaped(line, shape, &nginx)?;
            let ts = at(start, end, b * BATCH + j, lines.len())?;
            m.insert("dt".into(), rfc3339(ts)?.into());
            serde_json::to_writer(&mut ndjson, &m)?;
            ndjson.push(b'\n');
        }
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&ndjson)?;
        bodies.push(gz.finish()?);
    }
    Ok((bodies, lines.len(), raw))
}

fn logs(args: &[String]) -> Result<()> {
    let [addr, key, file, shape, days] = args else {
        bail!("usage: footprint logs ADDR KEY FILE SHAPE DAYS");
    };
    let (bodies, lines, raw) = log_bodies(Path::new(file), shape, days.parse()?)?;
    let headers = format!(
        "Authorization: Bearer {key}\r\nContent-Type: application/x-ndjson\r\nContent-Encoding: gzip\r\n"
    );
    let started = Instant::now();
    let throttled = send_all(addr, "/", &headers, bodies.len(), |i| Ok(bodies[i].clone()))?;
    let secs = started.elapsed().as_secs_f64();
    out(&format!("lines={lines}"))?;
    out(&format!("raw_bytes={raw}"))?;
    out(&format!("accept_secs={secs:.3}"))?;
    out(&format!("accept_rows_per_s={:.0}", ratio(lines, secs)))?;
    out(&format!("throttled={throttled}"))
}

struct ErrorRun {
    project: String,
    key: String,
    event: Map<String, Value>,
    issues: usize,
    per_issue: usize,
    start: i64,
    end: i64,
}

impl ErrorRun {
    /// Event `i`: issue `i % issues`, so each issue's events spread over
    /// the whole span, in one of three releases and two environments.
    fn event(&self, i: usize) -> Result<Vec<u8>> {
        let n = self.issues * self.per_issue;
        let id = format!("{:032x}", i + 1);
        let mut e = self.event.clone();
        let ts = at(self.start, self.end, i, n)?;
        e.insert("event_id".into(), id.clone().into());
        e.insert("timestamp".into(), rfc3339(ts)?.into());
        e.insert(
            "fingerprint".into(),
            json!([format!("footprint-{}", i % self.issues)]),
        );
        e.insert("release".into(), format!("app@1.0.{}", i % 3).into());
        let env = if i.is_multiple_of(10) {
            "staging"
        } else {
            "production"
        };
        e.insert("environment".into(), env.into());
        let payload = serde_json::to_vec(&e)?;
        let mut body = serde_json::to_vec(&json!({ "event_id": id }))?;
        body.push(b'\n');
        let item = json!({ "type": "event", "length": payload.len() });
        serde_json::to_writer(&mut body, &item)?;
        body.push(b'\n');
        body.extend_from_slice(&payload);
        body.push(b'\n');
        Ok(body)
    }
}

/// The event JSON of a captured envelope: the payload of its `event` item.
fn captured_event(envelope: &Path) -> Result<Map<String, Value>> {
    let text = std::fs::read_to_string(envelope)?;
    let mut lines = text.lines().skip(1);
    while let Some(header) = lines.next() {
        let header: Value = serde_json::from_str(header)?;
        let payload = lines.next().context("an item header without a payload")?;
        if header["type"] == "event" {
            return match serde_json::from_str(payload)? {
                Value::Object(m) => Ok(m),
                _ => bail!("the event is not an object"),
            };
        }
    }
    bail!("no event item in {}", envelope.display())
}

fn errors(args: &[String]) -> Result<()> {
    let [addr, key, project, envelope, issues, per_issue, days] = args else {
        bail!("usage: footprint errors ADDR KEY PROJECT ENVELOPE ISSUES PER_ISSUE DAYS");
    };
    let (start, end) = span(days.parse()?);
    let run = ErrorRun {
        project: project.clone(),
        key: key.clone(),
        event: captured_event(Path::new(envelope))?,
        issues: issues.parse()?,
        per_issue: per_issue.parse()?,
        start,
        end,
    };
    let n = run.issues * run.per_issue;
    let path = format!(
        "/api/{}/envelope/?sentry_version=7&sentry_key={}",
        run.project, run.key
    );
    let headers = "Content-Type: application/x-sentry-envelope\r\n";
    let raw = serde_json::to_vec(&run.event)?.len();
    let started = Instant::now();
    let throttled = send_all(addr, &path, headers, n, |i| run.event(i))?;
    let secs = started.elapsed().as_secs_f64();
    out(&format!("events={n}"))?;
    out(&format!("raw_event_bytes={raw}"))?;
    out(&format!("accept_secs={secs:.3}"))?;
    out(&format!("accept_events_per_s={:.0}", ratio(n, secs)))?;
    out(&format!("throttled={throttled}"))
}

/// The day index directories of a data directory, oldest first.
fn day_dirs(data: &Path) -> Result<Vec<PathBuf>> {
    let mut days = Vec::new();
    for entry in std::fs::read_dir(davy::logs::dir(data))? {
        let path = entry?.path();
        if day_of(&path).is_ok() {
            days.push(path);
        }
    }
    days.sort();
    Ok(days)
}

fn add_fields(totals: &mut BTreeMap<String, u64>, usage: &PerFieldSpaceUsage) {
    for f in usage.fields() {
        *totals.entry(f.field_name().to_owned()).or_default() += f.total().get_bytes();
    }
}

/// Bytes per field over every day, as tantivy counts them: the term
/// dictionary, postings, positions, fast fields and norms of a field, and
/// the compressed document store on its own.
fn inspect(args: &[String]) -> Result<()> {
    let [data] = args else {
        bail!("usage: footprint inspect DATA");
    };
    let (mut fields, mut store, mut total, mut docs, mut segments) = (BTreeMap::new(), 0, 0, 0, 0);
    let days = day_dirs(Path::new(data))?;
    for day in &days {
        let index = Index::open_in_dir(day)?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        let usage = reader.searcher().space_usage()?;
        for s in usage.segments() {
            for part in [
                s.termdict(),
                s.postings(),
                s.positions(),
                s.fast_fields(),
                s.fieldnorms(),
            ] {
                add_fields(&mut fields, part);
            }
            store += s.store().total().get_bytes();
            docs += u64::from(s.num_docs());
            segments += 1;
        }
        total += usage.total().get_bytes();
    }
    out(&format!("day_indexes={}", days.len()))?;
    out(&format!("docs={docs}"))?;
    out(&format!("segments={segments}"))?;
    out(&format!("tantivy_bytes={total}"))?;
    out(&format!("store_bytes={store}"))?;
    for (name, bytes) in fields {
        out(&format!("field_{name}_bytes={bytes}"))?;
    }
    Ok(())
}

fn merge(args: &[String]) -> Result<()> {
    let [data] = args else {
        bail!("usage: footprint merge DATA");
    };
    let started = Instant::now();
    for day in day_dirs(Path::new(data))? {
        let index = Index::open_in_dir(&day)?;
        let ids = index.searchable_segment_ids()?;
        let mut writer: IndexWriter<TantivyDocument> =
            index.writer_with_num_threads(1, 64_000_000)?;
        if ids.len() > 1 {
            writer.merge(&ids).wait()?;
        }
        writer.garbage_collect_files().wait()?;
        writer.wait_merging_threads()?;
    }
    out(&format!(
        "merge_secs={:.3}",
        started.elapsed().as_secs_f64()
    ))
}

/// The UTC date of a day index directory.
fn day_of(dir: &Path) -> Result<jiff::civil::Date> {
    let name = dir.file_name().context("a day directory has a name")?;
    Ok(name.to_string_lossy().parse()?)
}

/// Runs the log writer's retention once and fails unless that one pass
/// kept exactly the days younger than DAYS, since a pass stops early when
/// it runs over its time budget.
fn retain(args: &[String]) -> Result<()> {
    let [data, days] = args else {
        bail!("usage: footprint retain DATA DAYS");
    };
    let data = Path::new(data);
    let keep: u32 = days.parse()?;
    let cutoff = davy::time::day_of(davy::time::now_ns() - i64::from(keep) * DAY_MS * 1_000_000);
    let mut expected = 0;
    for day in day_dirs(data)? {
        expected += usize::from(day_of(&day)? >= cutoff);
    }
    let started = Instant::now();
    let writer = davy::logs::store::LogWriter::start(
        davy::logs::dir(data),
        keep,
        davy::logs::store::no_overrides(),
    )?;
    drop(writer.queue);
    writer
        .thread
        .join()
        .map_err(|_panic| anyhow!("the log writer panicked"))?;
    let secs = started.elapsed().as_secs_f64();
    let kept = day_dirs(data)?.len();
    if kept != expected {
        bail!("retention kept {kept} days, expected {expected}: the pass stopped early");
    }
    out(&format!("retention_secs={secs:.3}"))
}

fn synth(args: &[String]) -> Result<()> {
    let [rows] = args else {
        bail!("usage: footprint synth ROWS");
    };
    let mut rng = pino::Rng::seeded();
    let mut w = std::io::BufWriter::new(std::io::stdout().lock());
    for i in 0..rows.parse()? {
        serde_json::to_writer(&mut w, &pino::row(&mut rng, i, 0)?)?;
        w.write_all(b"\n")?;
    }
    w.flush()?;
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (command, rest) = args
        .split_first()
        .context("usage: see examples/footprint.rs")?;
    match command.as_str() {
        "synth" => synth(rest),
        "logs" => logs(rest),
        "errors" => errors(rest),
        "inspect" => inspect(rest),
        "merge" => merge(rest),
        "retain" => retain(rest),
        other => bail!("unknown command {other}, see examples/footprint.rs"),
    }
}
