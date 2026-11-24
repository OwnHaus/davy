use super::{LogLevel, LogRecord, LogSource, ORIGINAL_TIMESTAMP, patterns};
use crate::time::{day_of, now_ns, rfc3339};
use anyhow::{Result, anyhow};
use jiff::civil::Date;
use serde::Serialize;
use serde_json::{Map, Value};
use std::{
    collections::{HashMap, HashSet},
    ops::Bound,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};
use tantivy::{
    Document, Index, IndexSettings, IndexWriter, Order, TantivyDocument, Term,
    collector::TopDocs,
    query::{BooleanQuery, EmptyQuery, Occur, Query, QueryParser, RangeQuery, TermQuery},
    schema::{FAST, Field, INDEXED, IndexRecordOption, STORED, STRING, Schema, TEXT},
    store::{Compressor, ZstdCompressor},
    tokenizer::TokenizerManager,
};

const DAY_NS: i64 = 86_400_000_000_000;
const MAX_OPEN_DAYS: usize = 3;
const COMMIT_EVERY: Duration = Duration::from_secs(1);
const COMMIT_AT_DOCS: usize = 100_000;
const IDLE_CLOSE: Duration = Duration::from_secs(600);
/// How often the log writer reads the retention settings and deletes what
/// expired, so a new setting holds within a minute.
const RETENTION_EVERY: Duration = Duration::from_secs(60);
/// How soon the log writer tries again after a pass failed, doubled after
/// each failure up to [`RETENTION_EVERY`]. Until the settings are read,
/// logs are timed by the server default.
const RETENTION_RETRY: Duration = Duration::from_secs(5);
/// How long one retention pass may spend deleting before it lets writes in.
const RETENTION_BUDGET: Duration = Duration::from_secs(1);
const WRITER_HEAP: usize = 64_000_000;
pub const QUEUE_ROWS: usize = 50_000;
pub const QUEUE_BYTES: usize = 128 * 1024 * 1024;
/// 100 years. A longer window would overflow the nanosecond cutoff.
pub const MAX_RETENTION_DAYS: u32 = crate::retention::MAX_DAYS;

#[derive(Clone, Copy)]
struct Fields {
    project: Field,
    ts: Field,
    observed: Field,
    level: Field,
    source: Field,
    service: Field,
    trace_id: Field,
    span_id: Field,
    message: Field,
    attributes: Field,
    /// Every string and number in `attributes`, at any depth, so a bare word
    /// finds a value without naming its path. Not stored. Days written before
    /// it existed lack it.
    values: Field,
}

fn schema() -> (Schema, Fields) {
    let mut b = Schema::builder();
    let f = Fields {
        project: b.add_u64_field("project", INDEXED | FAST | STORED),
        ts: b.add_i64_field("ts", INDEXED | FAST | STORED),
        observed: b.add_i64_field("observed", STORED),
        level: b.add_u64_field("level", INDEXED | FAST | STORED),
        source: b.add_u64_field("source", STORED),
        service: b.add_text_field("service", STRING | STORED),
        trace_id: b.add_text_field("trace_id", STRING | STORED),
        span_id: b.add_text_field("span_id", STRING | STORED),
        message: b.add_text_field("message", TEXT | STORED),
        attributes: b.add_json_field("attributes", TEXT | STORED),
        values: b.add_text_field("values", TEXT),
    };
    // The id of the record's pattern, [`patterns::OTHER`] past the bound.
    // Days written before it existed lack it, so it is looked up by name in
    // each day: see [`pattern_field`].
    b.add_u64_field("pattern", INDEXED | STORED);
    (b.build(), f)
}

/// Whether a day index has [`Fields::values`].
fn has_values(index: &Index) -> bool {
    index.schema().get_field("values").is_ok()
}

/// The `pattern` field as the day index numbers it, if it has the field.
fn pattern_field(index: &Index) -> Option<Field> {
    index.schema().get_field("pattern").ok()
}

/// The day `name` is the directory of: `YYYY-MM-DD`, the one name of each
/// date. jiff also reads `+002026-01-05`, and prints a year past 9999 or
/// before 0 with six digits, so a day has exactly 10 characters.
fn parse_day(name: &str) -> Option<Date> {
    let day: Date = name.parse().ok()?;
    (name.len() == 10 && day.to_string() == name).then_some(day)
}

fn open_index(root: &Path, day: Date) -> Result<Index> {
    let dir = root.join(day.to_string());
    // A stray in the day's place fails the write rather than take its logs.
    if day_dirs::real_dir(&dir)? && dir.join("meta.json").exists() {
        return Ok(Index::open_in_dir(&dir)?);
    }
    std::fs::create_dir_all(&dir)?;
    Ok(Index::builder()
        .schema(schema().0)
        .settings(IndexSettings {
            docstore_compression: Compressor::Zstd(ZstdCompressor::default()),
            ..Default::default()
        })
        .create_in_dir(&dir)?)
}

fn document(
    f: &Fields,
    r: LogRecord,
    values: bool,
    pattern: Option<(Field, u64)>,
) -> TantivyDocument {
    let mut d = TantivyDocument::default();
    d.add_u64(f.project, r.project_id.cast_unsigned());
    d.add_i64(f.ts, r.timestamp);
    d.add_i64(f.observed, r.observed_at);
    d.add_u64(f.level, r.level as u64);
    d.add_u64(f.source, r.source.code());
    if let Some((field, id)) = pattern {
        d.add_u64(field, id);
    }
    if let Some(s) = r.service {
        d.add_text(f.service, s);
    }
    if let Some(s) = r.trace_id {
        d.add_text(f.trace_id, s);
    }
    if let Some(s) = r.span_id {
        d.add_text(f.span_id, s);
    }
    d.add_text(f.message, r.message);
    if values {
        add_values(&mut d, f.values, &r.attributes);
    }
    d.add_object(
        f.attributes,
        r.attributes
            .into_iter()
            .map(|(k, v)| (k, v.into()))
            .collect(),
    );
    d
}

/// Adds each string and number in `attributes` to `field`. A stack and not
/// recursion, because attributes nest as deep as the ingest allows.
fn add_values(d: &mut TantivyDocument, field: Field, attributes: &Map<String, Value>) {
    let mut stack: Vec<&Value> = attributes.values().collect();
    while let Some(v) = stack.pop() {
        match v {
            Value::String(s) => d.add_text(field, s),
            Value::Number(n) => d.add_text(field, n.to_string()),
            Value::Array(a) => stack.extend(a),
            Value::Object(m) => stack.extend(m.values()),
            Value::Bool(_) | Value::Null => {}
        }
    }
}

struct OpenDay {
    writer: IndexWriter,
    values: bool,
    /// The day's own `pattern` field, `None` in a day written before it.
    pattern: Option<Field>,
    last_write: Instant,
    dirty: bool,
}

pub(super) struct State {
    root: PathBuf,
    fields: Fields,
    policy: retention::Policy,
    overrides: Overrides,
    /// What retention last deleted from each day.
    swept: HashMap<Date, retention::Cut>,
    /// The strays retention has reported, so each is reported once.
    strays: HashSet<String>,
    open: HashMap<Date, OpenDay>,
    uncommitted: usize,
    tallies: Arc<Mutex<tally::Days>>,
}

impl State {
    pub(super) fn new(root: PathBuf, retention_days: u32, overrides: Overrides) -> Self {
        let tallies = tally::register(&root);
        Self {
            root,
            tallies,
            fields: schema().1,
            policy: retention::Policy::new(retention_days, HashMap::new()),
            overrides,
            swept: HashMap::new(),
            strays: HashSet::new(),
            open: HashMap::new(),
            uncommitted: 0,
        }
    }

    fn settle(&self, r: &mut LogRecord) {
        let days = self.policy.days(r.project_id.cast_unsigned());
        let oldest = r.observed_at - DAY_NS * i64::from(days);
        if r.timestamp < oldest || r.timestamp > r.observed_at + DAY_NS {
            r.attributes
                .insert(ORIGINAL_TIMESTAMP.into(), r.timestamp.into());
            r.timestamp = r.observed_at;
        }
    }

    fn writer(&mut self, day: Date) -> Result<&mut OpenDay> {
        if !self.open.contains_key(&day) {
            if self.open.len() >= MAX_OPEN_DAYS
                && let Some(oldest) = self
                    .open
                    .iter()
                    .min_by_key(|(_, o)| o.last_write)
                    .map(|(d, _)| *d)
            {
                self.close(oldest)?;
            }
            let index = open_index(&self.root, day)?;
            let values = has_values(&index);
            let pattern = pattern_field(&index);
            let writer = index.writer_with_num_threads(1, WRITER_HEAP)?;
            self.open.insert(
                day,
                OpenDay {
                    writer,
                    values,
                    pattern,
                    last_write: Instant::now(),
                    dirty: false,
                },
            );
        }
        Ok(self.open.get_mut(&day).expect("inserted above"))
    }

    pub(super) fn add(&mut self, batch: Vec<LogRecord>) -> Result<()> {
        let fields = self.fields;
        let (mut failed, mut first) = (HashSet::new(), None);
        for mut r in batch {
            self.settle(&mut r);
            let day = day_of(r.timestamp);
            if failed.contains(&day) {
                continue;
            }
            // `settle` judges a log by when it was observed, so one that
            // waited in the queue can land in a day retention has gone
            // through already. The next pass checks the day again.
            self.swept.remove(&day);
            // A day that cannot be opened, such as a stray in its place,
            // loses its own logs, not those of the other days. Counted once
            // the day is open, so a day that fails to open counts nothing. A
            // day index created before patterns has no field to stamp, so
            // its records are not counted either: a report would list ids
            // that find none of them.
            let stamp = match self.writer(day) {
                Ok(open) => open.pattern,
                Err(e) => {
                    failed.insert(day);
                    first.get_or_insert(e);
                    continue;
                }
            };
            let id = stamp.map(|field| (field, self.tally(day, &r)));
            let open = self.writer(day)?;
            if let Err(e) = open
                .writer
                .add_document(document(&fields, r, open.values, id))
            {
                self.discard(day);
                return Err(e.into());
            }
            open.last_write = Instant::now();
            open.dirty = true;
            self.uncommitted += 1;
        }
        if self.uncommitted >= COMMIT_AT_DOCS {
            self.commit()?;
        }
        first.map_or(Ok(()), Err)
    }

    pub(super) fn commit(&mut self) -> Result<()> {
        let (mut committed, mut failed) = (Vec::new(), Vec::new());
        for (day, open) in self.open.iter_mut().filter(|(_, o)| o.dirty) {
            match open.writer.commit() {
                Ok(_) => {
                    open.dirty = false;
                    committed.push(*day);
                }
                Err(e) => failed.push((*day, e)),
            }
        }
        for day in committed {
            self.settle_tally(day);
        }
        self.uncommitted = 0;
        let mut first = None;
        for (day, e) in failed {
            self.discard(day);
            first.get_or_insert(e);
        }
        first.map_or(Ok(()), |e| Err(e.into()))
    }

    // A failed commit leaves tantivy's IndexWriter without indexing workers, so the
    // next add_document would block forever. Drop it and reopen on the next write.
    fn discard(&mut self, day: Date) {
        self.open.remove(&day);
        self.discard_tally(day);
    }

    fn close(&mut self, day: Date) -> Result<()> {
        let Some(mut open) = self.open.remove(&day) else {
            self.close_tally(day);
            return Ok(());
        };
        let committed = open.writer.commit();
        if committed.is_ok() {
            self.settle_tally(day);
        } else {
            self.discard_tally(day);
        }
        self.close_tally(day);
        committed?;
        open.writer.wait_merging_threads()?;
        Ok(())
    }

    fn close_idle(&mut self) -> Result<()> {
        let idle: Vec<_> = self
            .open
            .iter()
            .filter(|(_, o)| o.last_write.elapsed() >= IDLE_CLOSE)
            .map(|(d, _)| *d)
            .collect();
        for day in idle {
            self.close(day)?;
        }
        Ok(())
    }

    pub(super) fn close_all(&mut self) -> Result<()> {
        let days: Vec<_> = self.open.keys().copied().collect();
        for day in days {
            self.close(day)?;
        }
        Ok(())
    }
}

pub struct LogWriter {
    pub queue: LogQueue,
    pub thread: thread::JoinHandle<()>,
}

impl LogWriter {
    /// Starts the writer of the day indexes in `root`. A project keeps
    /// `retention_days` of logs unless `overrides` names it.
    pub fn start(root: PathBuf, retention_days: u32, overrides: Overrides) -> Result<Self> {
        anyhow::ensure!(
            (1..=MAX_RETENTION_DAYS).contains(&retention_days),
            "log retention must be 1 to {MAX_RETENTION_DAYS} days"
        );
        std::fs::create_dir_all(&root)?;
        let (queue, inbox) = LogQueue::bounded(QUEUE_ROWS, QUEUE_BYTES);
        let mut state = State::new(root, retention_days, overrides);
        let thread = thread::spawn(move || {
            #[expect(
                clippy::print_stderr,
                reason = "the server has no logger yet, stderr is its log"
            )]
            let report = |what: &str, r: Result<()>| {
                if let Err(e) = r {
                    eprintln!("log writer: {what}: {e:#}");
                }
            };
            let mut next_commit = Instant::now() + COMMIT_EVERY;
            let mut next_flush = Instant::now() + tally::FLUSH_EVERY;
            let mut next_retention = Instant::now();
            let mut retry = RETENTION_RETRY;
            loop {
                // Retention goes first, so the first batch is settled with
                // each project's own retention.
                if Instant::now() >= next_retention {
                    let pass = state.retention(now_ns(), RETENTION_BUDGET);
                    let wait = if pass.is_ok() {
                        retry = RETENTION_RETRY;
                        RETENTION_EVERY
                    } else {
                        let wait = retry;
                        retry = (retry * 2).min(RETENTION_EVERY);
                        wait
                    };
                    next_retention = Instant::now() + wait;
                    report("retention", pass);
                }
                let wait = next_commit.saturating_duration_since(Instant::now());
                match inbox.recv_timeout(wait) {
                    Ok((batch, _held)) => report("write", state.add(batch)),
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
                let now = Instant::now();
                if now >= next_commit {
                    report("commit", state.commit());
                    report("close idle", state.close_idle());
                    next_commit = now + COMMIT_EVERY;
                }
                if now >= next_flush {
                    state.flush_tallies(now);
                    next_flush = now + tally::FLUSH_EVERY;
                }
            }
            report("shutdown", state.close_all());
        });
        Ok(Self { queue, thread })
    }
}

#[derive(Debug, Clone)]
pub struct LogQuery {
    pub project: Option<i64>,
    pub since: i64,
    pub until: i64,
    pub min_level: Option<LogLevel>,
    pub text: Option<String>,
    /// Only logs of this pattern, see [`patterns::of`].
    pub pattern: Option<u64>,
    pub limit: usize,
    /// The newest hits at exactly `until` to pass over without reading them,
    /// at most: those an earlier page showed.
    pub skip: usize,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct LogHit {
    pub timestamp: String,
    pub timestamp_ns: i64,
    pub project_id: i64,
    pub source: LogSource,
    pub level: LogLevel,
    pub service: Option<String>,
    pub message: String,
    pub trace_id: Option<String>,
    pub span_id: Option<String>,
    /// As [`patterns::show`] prints it, `None` in a day written before
    /// patterns.
    pub pattern: Option<String>,
    pub attributes: Map<String, Value>,
}

/// The newest `q.limit` hits, oldest first.
pub fn search(root: &Path, q: &LogQuery) -> Result<Vec<LogHit>> {
    retrying(|| search_once(root, q))
}

/// Runs `f` a second time when its first pass met a day that retention
/// deleted between the `meta.json` check and the open. tantivy reports that
/// only as text, so a pass that no longer sees the day is the way to recover.
fn retrying<T>(f: impl Fn() -> Result<T>) -> Result<T> {
    f().or_else(|e| {
        let missing = format!("{e:#}");
        if missing.contains("does not exist") || missing.contains("No such file") {
            f()
        } else {
            Err(e)
        }
    })
}

impl Day {
    /// What `q` matches in this day, the limit aside.
    fn matching(&self, f: &Fields, q: &LogQuery) -> Result<BooleanQuery> {
        let mut clauses: Vec<(Occur, Box<dyn Query>)> = vec![(
            Occur::Must,
            Box::new(RangeQuery::new(
                Bound::Included(Term::from_field_i64(f.ts, q.since)),
                Bound::Included(Term::from_field_i64(f.ts, q.until)),
            )),
        )];
        if let Some(p) = q.project {
            clauses.push((
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_u64(f.project, p.cast_unsigned()),
                    IndexRecordOption::Basic,
                )),
            ));
        }
        if let Some(level) = q.min_level {
            clauses.push((
                Occur::Must,
                Box::new(RangeQuery::new(
                    Bound::Included(Term::from_field_u64(f.level, level as u64)),
                    Bound::Unbounded,
                )),
            ));
        }
        if let Some(id) = q.pattern {
            clauses.push(if let Some(field) = pattern_field(&self.index) {
                let term = Term::from_field_u64(field, id);
                (
                    Occur::Must,
                    Box::new(TermQuery::new(term, IndexRecordOption::Basic)),
                )
            } else {
                (Occur::Must, Box::new(EmptyQuery))
            });
        }
        if let Some(text) = q.text.as_deref().filter(|t| !t.trim().is_empty()) {
            let words = if has_values(&self.index) {
                f.values
            } else {
                f.attributes
            };
            let parser = QueryParser::for_index(&self.index, vec![f.message, words]);
            let parsed = parser
                .parse_query(text)
                .map_err(|e| anyhow!("invalid query {text:?}: {e}"))?;
            clauses.push((Occur::Must, parsed));
        }
        Ok(BooleanQuery::new(clauses))
    }
}

/// Why `text` is not a query `search` takes, if it is not.
pub fn check_text(text: &str) -> Result<(), String> {
    let (schema, f) = schema();
    let parser = QueryParser::new(
        schema,
        vec![f.message, f.values],
        TokenizerManager::default(),
    );
    parser
        .parse_query(text)
        .map(drop)
        .map_err(|e| e.to_string())
}

fn search_once(root: &Path, q: &LogQuery) -> Result<Vec<LogHit>> {
    let f = schema().1;
    let mut hits = Vec::new();
    let mut skip = q.skip;
    for day in days(root, q.since, q.until)? {
        if hits.len() >= q.limit {
            break;
        }
        let day = day?;
        let top = day.searcher.search(
            &day.matching(&f, q)?,
            &TopDocs::with_limit(q.limit - hits.len() + skip)
                .order_by_fast_field::<i64>("ts", Order::Desc),
        )?;
        for (ts, address) in top {
            if skip > 0 && ts == Some(q.until) {
                skip -= 1;
                continue;
            }
            skip = 0;
            if hits.len() == q.limit {
                break;
            }
            let doc: TantivyDocument = day.searcher.doc(address)?;
            hits.push(hit(&day.index.schema(), &f, &doc)?);
        }
    }
    hits.reverse();
    Ok(hits)
}

fn hit(schema: &Schema, f: &Fields, doc: &TantivyDocument) -> Result<LogHit> {
    let v: Value = serde_json::from_str(&doc.to_json(schema))?;
    let first = |field: Field| {
        v.get(schema.get_field_name(field))
            .and_then(|a| a.get(0))
            .cloned()
    };
    let int = |field: Field| first(field).and_then(|v| v.as_i64()).unwrap_or(0);
    let uint = |field: Field| first(field).and_then(|v| v.as_u64()).unwrap_or(0);
    let string = |field: Field| first(field).and_then(|v| v.as_str().map(str::to_owned));
    let ts = int(f.ts);
    Ok(LogHit {
        timestamp: rfc3339(ts),
        timestamp_ns: ts,
        project_id: uint(f.project).cast_signed(),
        source: LogSource::from_code(uint(f.source)),
        level: LogLevel::from_code(uint(f.level)),
        service: string(f.service),
        message: string(f.message).unwrap_or_default(),
        trace_id: string(f.trace_id),
        span_id: string(f.span_id),
        // By name: in a day without the field its id names another one.
        pattern: v
            .get("pattern")
            .and_then(|a| a.get(0)?.as_u64())
            .map(patterns::show),
        attributes: match first(f.attributes) {
            Some(Value::Object(m)) => m,
            _ => Map::new(),
        },
    })
}

#[path = "days.rs"]
mod days;

#[path = "day_dirs.rs"]
mod day_dirs;
pub use day_dirs::{Stray, strays};

#[path = "retention.rs"]
mod retention;
use days::{Day, days};
pub use retention::{Overrides, no_overrides};

#[path = "histogram.rs"]
mod histogram;
pub use histogram::{Histogram, LogCounts, MAX_BUCKETS, histogram};

#[path = "queue.rs"]
mod queue;
pub use queue::{Held, LogInbox, LogQueue, QueueError};

#[path = "tally.rs"]
mod tally;

#[path = "pattern_report.rs"]
mod pattern_report;
pub use pattern_report::{
    MAX_MERGED, MAX_PATTERN_ROWS, MAX_REPORT_DAYS, PatternQuery, PatternReport, PatternRow,
    top_patterns,
};

#[cfg(test)]
#[path = "values_tests.rs"]
mod values_tests;
