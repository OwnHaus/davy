use super::{LogLevel, LogRecord, LogSource, ORIGINAL_TIMESTAMP};
use crate::{
    heap,
    time::{day_of, now_ns, rfc3339},
};
use anyhow::{Context, Result, anyhow};
use jiff::civil::Date;
use serde::Serialize;
use serde_json::{Map, Value};
use std::{
    collections::HashMap,
    ops::Bound,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering::SeqCst},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use tantivy::{
    Document, Index, IndexReader, IndexSettings, IndexWriter, Order, ReloadPolicy, Searcher,
    TantivyDocument, Term,
    collector::TopDocs,
    query::{BooleanQuery, Occur, Query, QueryParser, RangeQuery, TermQuery},
    schema::{FAST, Field, INDEXED, IndexRecordOption, STORED, STRING, Schema, TEXT},
    store::{Compressor, ZstdCompressor},
    tokenizer::TokenizerManager,
};

const DAY_NS: i64 = 86_400_000_000_000;
const MAX_OPEN_DAYS: usize = 3;
const COMMIT_EVERY: Duration = Duration::from_secs(1);
const COMMIT_AT_DOCS: usize = 100_000;
const IDLE_CLOSE: Duration = Duration::from_secs(600);
const RETENTION_EVERY: Duration = Duration::from_secs(3600);
const WRITER_HEAP: usize = 64_000_000;
pub const QUEUE_ROWS: usize = 50_000;
pub const QUEUE_BYTES: usize = 128 * 1024 * 1024;
/// 100 years. A longer window would overflow the nanosecond cutoff.
pub const MAX_RETENTION_DAYS: u32 = 36_500;

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
    };
    (b.build(), f)
}

fn parse_day(name: &str) -> Option<Date> {
    (name.len() == 10).then(|| name.parse().ok()).flatten()
}

fn open_index(root: &Path, day: Date) -> Result<Index> {
    let dir = root.join(day.to_string());
    if dir.join("meta.json").exists() {
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

fn document(f: &Fields, r: LogRecord) -> TantivyDocument {
    let mut d = TantivyDocument::default();
    d.add_u64(f.project, r.project_id.cast_unsigned());
    d.add_i64(f.ts, r.timestamp);
    d.add_i64(f.observed, r.observed_at);
    d.add_u64(f.level, r.level as u64);
    d.add_u64(f.source, r.source.code());
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
    d.add_object(
        f.attributes,
        r.attributes
            .into_iter()
            .map(|(k, v)| (k, v.into()))
            .collect(),
    );
    d
}

struct OpenDay {
    writer: IndexWriter,
    last_write: Instant,
    dirty: bool,
}

pub(super) struct State {
    root: PathBuf,
    fields: Fields,
    retention_days: u32,
    open: HashMap<Date, OpenDay>,
    uncommitted: usize,
}

impl State {
    pub(super) fn new(root: PathBuf, retention_days: u32) -> Self {
        Self {
            root,
            fields: schema().1,
            retention_days,
            open: HashMap::new(),
            uncommitted: 0,
        }
    }

    fn settle(&self, r: &mut LogRecord) {
        let oldest = r.observed_at - DAY_NS * i64::from(self.retention_days);
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
            let writer = open_index(&self.root, day)?.writer_with_num_threads(1, WRITER_HEAP)?;
            self.open.insert(
                day,
                OpenDay {
                    writer,
                    last_write: Instant::now(),
                    dirty: false,
                },
            );
        }
        Ok(self.open.get_mut(&day).expect("inserted above"))
    }

    pub(super) fn add(&mut self, batch: Vec<LogRecord>) -> Result<()> {
        let fields = self.fields;
        for mut r in batch {
            self.settle(&mut r);
            let day = day_of(r.timestamp);
            let open = self.writer(day)?;
            if let Err(e) = open.writer.add_document(document(&fields, r)) {
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
        Ok(())
    }

    pub(super) fn commit(&mut self) -> Result<()> {
        let mut failed = Vec::new();
        for (day, open) in self.open.iter_mut().filter(|(_, o)| o.dirty) {
            match open.writer.commit() {
                Ok(_) => open.dirty = false,
                Err(e) => failed.push((*day, e)),
            }
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
    }

    fn close(&mut self, day: Date) -> Result<()> {
        if let Some(mut open) = self.open.remove(&day) {
            open.writer.commit()?;
            open.writer.wait_merging_threads()?;
        }
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

    fn retention(&mut self) -> Result<()> {
        let cutoff = day_of(now_ns() - DAY_NS * i64::from(self.retention_days));
        let trash = self.root.join(".trash");
        for entry in std::fs::read_dir(&self.root)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(day) = parse_day(&name) else {
                continue;
            };
            if day >= cutoff {
                continue;
            }
            self.close(day)?;
            std::fs::create_dir_all(&trash)?;
            let target = trash.join(format!("{name}-{}", now_ns()));
            std::fs::rename(entry.path(), &target)?;
            std::fs::remove_dir_all(&target)?;
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

pub enum QueueError {
    /// The queue has no room for the batch now.
    Full,
    /// The batch is larger than the whole queue and never fits.
    TooLarge,
    Stopped,
}

/// What the log queue holds, counted in rows and in the heap the records
/// take, from the moment a batch is queued until the writer is done with it.
#[derive(Default)]
struct Load {
    rows: AtomicUsize,
    bytes: AtomicUsize,
}

/// A batch's share of the [`Load`], given back when it is dropped: after the
/// writer has indexed the batch, or when the batch is refused or lost with
/// the channel.
pub struct Held {
    load: Arc<Load>,
    rows: usize,
    bytes: usize,
}

impl Drop for Held {
    fn drop(&mut self) {
        self.load.rows.fetch_sub(self.rows, SeqCst);
        self.load.bytes.fetch_sub(self.bytes, SeqCst);
    }
}

/// Ingest side of the log queue. Bounded in rows and bytes rather than
/// batches, because a batch can hold one record or thousands and memory
/// follows the records.
#[derive(Clone)]
pub struct LogQueue {
    sender: mpsc::Sender<(Vec<LogRecord>, Held)>,
    load: Arc<Load>,
    rows: usize,
    bytes: usize,
}

pub struct LogInbox {
    receiver: mpsc::Receiver<(Vec<LogRecord>, Held)>,
}

impl LogQueue {
    pub fn bounded(rows: usize, bytes: usize) -> (Self, LogInbox) {
        let (sender, receiver) = mpsc::channel();
        let queue = Self {
            sender,
            load: Arc::default(),
            rows,
            bytes,
        };
        (queue, LogInbox { receiver })
    }

    pub fn push(&self, batch: Vec<LogRecord>) -> Result<(), QueueError> {
        let rows = batch.len();
        if rows == 0 {
            return Ok(());
        }
        let bytes = batch.iter().map(heap::of_record).sum();
        if rows > self.rows || bytes > self.bytes {
            return Err(QueueError::TooLarge);
        }
        let rows_before = self.load.rows.fetch_add(rows, SeqCst);
        let bytes_before = self.load.bytes.fetch_add(bytes, SeqCst);
        let held = Held {
            load: Arc::clone(&self.load),
            rows,
            bytes,
        };
        if rows_before + rows > self.rows || bytes_before + bytes > self.bytes {
            return Err(QueueError::Full);
        }
        self.sender
            .send((batch, held))
            .map_err(|mpsc::SendError(_)| QueueError::Stopped)
    }
}

impl LogInbox {
    /// The next batch, and its share of the queue to drop once it is written.
    pub fn recv_timeout(
        &self,
        wait: Duration,
    ) -> Result<(Vec<LogRecord>, Held), mpsc::RecvTimeoutError> {
        self.receiver.recv_timeout(wait)
    }

    /// The next batch, giving back its share of the queue at once.
    pub fn try_recv(&self) -> Option<Vec<LogRecord>> {
        self.receiver.try_recv().ok().map(|(batch, _)| batch)
    }
}

pub struct LogWriter {
    pub queue: LogQueue,
    pub thread: thread::JoinHandle<()>,
}

impl LogWriter {
    pub fn start(root: PathBuf, retention_days: u32) -> Result<Self> {
        anyhow::ensure!(
            (1..=MAX_RETENTION_DAYS).contains(&retention_days),
            "log retention must be 1 to {MAX_RETENTION_DAYS} days"
        );
        std::fs::create_dir_all(&root)?;
        let (queue, inbox) = LogQueue::bounded(QUEUE_ROWS, QUEUE_BYTES);
        let mut state = State::new(root, retention_days);
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
            let mut next_retention = Instant::now();
            loop {
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
                if now >= next_retention {
                    report("retention", state.retention());
                    next_retention = now + RETENTION_EVERY;
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

/// The searcher of each day index from `since` to `until`, newest first.
fn days(root: &Path, since: i64, until: i64) -> Result<impl Iterator<Item = Result<Day>>> {
    let (first, last) = (day_of(since), day_of(until));
    let mut days: Vec<Date> = match std::fs::read_dir(root) {
        Ok(entries) => entries
            .filter_map(|e| parse_day(&e.ok()?.file_name().to_string_lossy()))
            .filter(|d| (first..=last).contains(d))
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e.into()),
    };
    days.sort_unstable_by(|a, b| b.cmp(a));
    let root = root.to_owned();
    Ok(days.into_iter().filter_map(move |day| {
        let dir = root.join(day.to_string());
        dir.join("meta.json").exists().then(|| Day::open(&dir))
    }))
}

/// One day index, read as it was when opened.
struct Day {
    index: Index,
    searcher: Searcher,
}

impl Day {
    fn open(dir: &Path) -> Result<Self> {
        let index = Index::open_in_dir(dir).with_context(|| format!("open {}", dir.display()))?;
        let reader: IndexReader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        let searcher = reader.searcher();
        Ok(Self { index, searcher })
    }

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
        if let Some(text) = q.text.as_deref().filter(|t| !t.trim().is_empty()) {
            let parser = QueryParser::for_index(&self.index, vec![f.message, f.attributes]);
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
        vec![f.message, f.attributes],
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
        attributes: match first(f.attributes) {
            Some(Value::Object(m)) => m,
            _ => Map::new(),
        },
    })
}

#[path = "histogram.rs"]
mod histogram;
pub use histogram::{Histogram, MAX_BUCKETS, histogram};

#[cfg(test)]
#[path = "queue_tests.rs"]
mod queue_tests;
