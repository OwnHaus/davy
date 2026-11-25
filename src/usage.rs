//! What each project sends Davy, how fast logs grow and when the disk fills
//! (`docs/design.md`, "Usage and disk forecast"). Ingest counts accepted
//! records and bytes in memory, and the writer adds them to `usage_daily`
//! every few seconds.

use crate::{
    logs::store::LogDays,
    store::{self, Job, Refused},
    time,
};
use anyhow::Result;
use jiff::{ToSpan as _, civil::Date};
use rusqlite::Connection;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

mod cli;
pub mod disk;
pub mod footprint;
pub mod forecast;
mod report;
pub mod stored;
pub use cli::{Args, run};
pub use report::{Prepared, Report, Request, prepare, report};

pub const MIGRATION: &str = "CREATE TABLE usage_daily(day TEXT NOT NULL, project_id INTEGER NOT NULL REFERENCES projects(id), signal TEXT NOT NULL, records INTEGER NOT NULL, bytes INTEGER NOT NULL, PRIMARY KEY(day, project_id, signal)) WITHOUT ROWID;";
/// Undoes [`MIGRATION`], for tests that run the migrations again.
#[cfg(test)]
pub const UNDO: &str = "DROP TABLE usage_daily;";

/// How often the counts in memory go to the writer. A crash loses the
/// counts not committed yet: those of the last 5 seconds, or more while
/// the writer is behind.
pub const FLUSH_EVERY: Duration = Duration::from_secs(5);
/// Counts in one write, so one job holds the writer only briefly.
pub const MAX_ROWS: usize = 500;
/// Old counts one sweep deletes, for the same reason.
pub const PRUNE_ROWS: i64 = 10_000;
const QUEUE_FULL_WAIT: Duration = Duration::from_millis(50);
/// Days of counts the sweep keeps, so a year compares with the one before.
pub const KEEP_DAYS: i64 = 400;

/// What a project sends. The names are what `usage_daily.signal` holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Signal {
    Errors,
    Logs,
    Sessions,
    Feedback,
    Checkins,
}

impl Signal {
    pub const ALL: [Self; 5] = [
        Self::Errors,
        Self::Logs,
        Self::Sessions,
        Self::Feedback,
        Self::Checkins,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Self::Errors => "errors",
            Self::Logs => "logs",
            Self::Sessions => "sessions",
            Self::Feedback => "feedback",
            Self::Checkins => "checkins",
        }
    }
    fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.name() == s)
    }
}

/// Records and their bytes after decompression, as received.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Count {
    pub records: u64,
    pub bytes: u64,
}

impl Count {
    pub fn new(records: usize, bytes: usize) -> Self {
        let wide = |n: usize| u64::try_from(n).unwrap_or(u64::MAX);
        Self {
            records: wide(records),
            bytes: wide(bytes),
        }
    }
    pub(crate) fn add(&mut self, other: Self) {
        self.records = self.records.saturating_add(other.records);
        self.bytes = self.bytes.saturating_add(other.bytes);
    }
}

/// What one request carries, counted while it is parsed and kept until
/// each part is accepted.
#[derive(Debug, Default)]
pub struct Tally([Count; 5]);

impl Tally {
    pub fn add(&mut self, signal: Signal, count: Count) {
        self.0[signal as usize].add(count);
    }
    pub fn get(&self, signal: Signal) -> Count {
        self.0[signal as usize]
    }
}

/// One day of one project's signal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub day: Date,
    pub project: i64,
    pub signal: Signal,
    pub count: Count,
}

/// Counts in memory until the next flush. Keys are a day, a project a key
/// authenticated and a signal, so the map stays small.
#[derive(Clone, Default)]
pub struct Counter(Arc<Mutex<HashMap<Key, Count>>>);

/// A day, a project and a signal.
type Key = (Date, i64, Signal);

impl Counter {
    /// Adds what was accepted now, on today's UTC day.
    pub fn add(&self, project: i64, signal: Signal, count: Count) {
        if count.records > 0 {
            let day = time::day_of(time::now_ns());
            self.put(day, project, signal, count);
        }
    }
    fn put(&self, day: Date, project: i64, signal: Signal, count: Count) {
        let mut map = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        map.entry((day, project, signal)).or_default().add(count);
    }
    /// Whether every count held is of `project`, and one is.
    pub fn only(&self, project: i64) -> bool {
        let map = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        !map.is_empty() && map.keys().all(|(_, p, _)| *p == project)
    }
    /// Takes every count out, sorted so tests can compare.
    pub fn take(&self) -> Vec<Row> {
        self.take_up_to(usize::MAX)
    }
    /// Takes the first `n` counts out, sorted, and leaves the rest.
    fn take_up_to(&self, n: usize) -> Vec<Row> {
        let mut map = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let mut rows: Vec<Row> = map
            .drain()
            .map(|((day, project, signal), count)| Row {
                day,
                project,
                signal,
                count,
            })
            .collect();
        rows.sort_by_key(|r| (r.day, r.project, r.signal as usize));
        for r in rows.split_off(n.min(rows.len())) {
            map.insert((r.day, r.project, r.signal), r.count);
        }
        rows
    }
    fn put_back(&self, rows: Vec<Row>) {
        for r in rows {
            self.put(r.day, r.project, r.signal, r.count);
        }
    }
}

/// Adds `rows` to `usage_daily`. A project deleted since it was counted is
/// skipped.
pub fn write(c: &Connection, rows: &[Row]) -> rusqlite::Result<()> {
    let mut q = c.prepare_cached(
        "INSERT INTO usage_daily(day, project_id, signal, records, bytes)
         SELECT ?1, id, ?3, ?4, ?5 FROM projects WHERE id=?2
         ON CONFLICT(day, project_id, signal) DO UPDATE
         SET records=records+excluded.records, bytes=bytes+excluded.bytes",
    )?;
    let narrow = |n: u64| i64::try_from(n).unwrap_or(i64::MAX);
    for r in rows {
        q.execute((
            r.day.to_string(),
            r.project,
            r.signal.name(),
            narrow(r.count.records),
            narrow(r.count.bytes),
        ))?;
    }
    Ok(())
}

/// Deletes the counts of days other than the [`KEEP_DAYS`] that end on
/// the day of `at`, at most [`PRUNE_ROWS`] of them, so a sweep after a
/// long outage holds the writer only briefly. The next sweep goes on.
pub fn prune(c: &Connection, at_ms: i64) -> rusqlite::Result<()> {
    let today = time::day_of(at_ms.saturating_mul(1_000_000));
    if let Ok(cutoff) = today.checked_sub(KEEP_DAYS.days()) {
        c.execute(
            "DELETE FROM usage_daily WHERE (day, project_id, signal) IN
             (SELECT day, project_id, signal FROM usage_daily WHERE day<=?1 LIMIT ?2)",
            (cutoff.to_string(), PRUNE_ROWS),
        )?;
    }
    Ok(())
}

/// Sends the counts and what the log writer stored and cut to the writer
/// every `period`. What the writer did not commit, because its queue was
/// full or the write failed, waits for the next tick. The task ends when
/// the writer stops, or between two sends once `stop` resolves, so a job
/// it sent is answered and one that failed is put back for the last flush.
pub async fn flush_every(
    counter: Counter,
    log_days: LogDays,
    sender: store::Sender,
    period: Duration,
    mut stop: tokio::sync::oneshot::Receiver<()>,
) {
    let mut ticks = tokio::time::interval(period);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = ticks.tick() => {}
            _ = &mut stop => return,
        }
        match send(&counter, &log_days, &sender).await {
            Ok(()) | Err(Refused::Full) => {}
            Err(Refused::Stopped) => return,
            Err(e @ Refused::Failed(_)) => {
                warn_held(&counter, "counts not stored, kept for the next try", &e);
            }
        }
    }
}

/// Sends what is left and waits until the writer committed it, for
/// shutdown. A full queue is tried again every 50 ms.
pub async fn flush(
    counter: &Counter,
    log_days: &LogDays,
    sender: &store::Sender,
) -> Result<(), Refused> {
    loop {
        match send(counter, log_days, sender).await {
            Err(Refused::Full) => tokio::time::sleep(QUEUE_FULL_WAIT).await,
            done => return done,
        }
    }
}

/// Reports counts that were not stored. When the counts held are those of
/// the project `davy` alone, the line goes to stderr only: it would be
/// counted in turn, and fail to be stored again.
pub fn warn_held(counter: &Counter, what: &str, e: &Refused) {
    let own = crate::self_report::project_id().is_some_and(|id| counter.only(id));
    let _quiet = own.then(crate::self_report::quiet);
    warn(what, e);
}

/// Reports counts that were not stored.
pub fn warn(what: &str, e: &Refused) {
    crate::self_report::warn("usage flusher", format_args!("{what}: {e}"));
}

/// Hands the counts, then what the log writer stored and cut, to the
/// writer, at most [`MAX_ROWS`] in one job, and waits until each job is
/// committed. A job the writer did not commit goes back where it came
/// from, before what was not sent yet.
async fn send(
    counter: &Counter,
    log_days: &LogDays,
    sender: &store::Sender,
) -> Result<(), Refused> {
    loop {
        let rows = counter.take_up_to(MAX_ROWS);
        if rows.is_empty() {
            break;
        }
        if let Err(e) = store::submit(sender, Job::Usage(rows.clone())).await {
            counter.put_back(rows);
            return Err(e);
        }
    }
    while let Some(flush) = log_days.take_up_to(MAX_ROWS) {
        if let Err(e) = store::submit(sender, Job::LogDays(flush.clone())).await {
            log_days.put_back(flush);
            return Err(e);
        }
    }
    Ok(())
}

/// A day of error events one project received.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ErrorDay {
    pub project: i64,
    /// When the project was made, ns.
    pub created: i64,
    /// The first day `usage_daily` holds error events of the project.
    pub first: Date,
    pub day: Date,
    pub count: Count,
}

/// Error events received and their bytes per project and accept day, from
/// the day `from` on: the forecast's events per day.
pub fn errors_since(c: &Connection, from: Date) -> Result<Vec<ErrorDay>> {
    let mut q = c.prepare(
        "SELECT u.project_id, p.created_at, f.first, u.day, u.records, u.bytes
         FROM usage_daily u JOIN projects p ON p.id=u.project_id
         JOIN (SELECT project_id, min(day) AS first FROM usage_daily
               WHERE signal='errors' GROUP BY project_id) f
           ON f.project_id=u.project_id
         WHERE u.day>=?1 AND u.signal='errors' ORDER BY u.day",
    )?;
    let rows = q.query_map([from.to_string()], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, i64>(4)?,
            r.get::<_, i64>(5)?,
        ))
    })?;
    let wide = |n: i64| u64::try_from(n).unwrap_or(0);
    let mut out = Vec::new();
    for row in rows {
        let (project, created_ms, first, day, records, bytes) = row?;
        out.push(ErrorDay {
            project,
            created: created_ms.saturating_mul(1_000_000),
            first: first.parse()?,
            day: day.parse()?,
            count: Count {
                records: wide(records),
                bytes: wide(bytes),
            },
        });
    }
    Ok(out)
}

/// Counted records and bytes per project and signal from the day `from`
/// to the day `to`, both included, of one project or of all.
pub fn between(
    c: &Connection,
    from: Date,
    to: Date,
    project: Option<i64>,
) -> Result<HashMap<(i64, Signal), Count>> {
    let mut q = c.prepare(
        "SELECT project_id, signal, SUM(records), SUM(bytes) FROM usage_daily
         WHERE day>=?1 AND day<=?2 AND (?3 IS NULL OR project_id=?3)
         GROUP BY project_id, signal",
    )?;
    let rows = q.query_map((from.to_string(), to.to_string(), project), |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, i64>(3)?,
        ))
    })?;
    let mut out = HashMap::new();
    for row in rows {
        let (project, signal, records, bytes) = row?;
        let wide = |n: i64| u64::try_from(n).unwrap_or(0);
        if let Some(signal) = Signal::parse(&signal) {
            out.insert(
                (project, signal),
                Count {
                    records: wide(records),
                    bytes: wide(bytes),
                },
            );
        }
    }
    Ok(out)
}

#[cfg(test)]
#[path = "usage/tests.rs"]
mod tests;
