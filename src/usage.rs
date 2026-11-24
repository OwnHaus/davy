//! What Davy stores per project, how fast it grows and when the disk fills
//! (`docs/design.md`, "Usage and disk forecast"). Ingest counts accepted
//! records and bytes in memory, and the writer adds them to `usage_daily`
//! every few seconds.

use crate::{
    store::{self, Job, Write},
    time,
};
use anyhow::Result;
use crossbeam_channel::TrySendError;
use jiff::{ToSpan as _, civil::Date};
use rusqlite::Connection;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

mod cli;
pub mod disk;
pub mod forecast;
mod report;
pub use cli::{Args, run};
pub use report::{Report, Request, report};

pub const MIGRATION: &str = "CREATE TABLE usage_daily(day TEXT NOT NULL, project_id INTEGER NOT NULL REFERENCES projects(id), signal TEXT NOT NULL, records INTEGER NOT NULL, bytes INTEGER NOT NULL, PRIMARY KEY(day, project_id, signal)) WITHOUT ROWID;";
/// Undoes [`MIGRATION`], for tests that run the migrations again.
#[cfg(test)]
pub const UNDO: &str = "DROP TABLE usage_daily;";

/// How often the counts in memory go to the writer, and so the most a
/// crash loses.
pub const FLUSH_EVERY: Duration = Duration::from_secs(5);
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
    fn add(&mut self, other: Self) {
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
    /// Takes every count out, sorted so tests can compare.
    pub fn take(&self) -> Vec<Row> {
        let map = std::mem::take(&mut *self.0.lock().unwrap_or_else(PoisonError::into_inner));
        let mut rows: Vec<Row> = map
            .into_iter()
            .map(|((day, project, signal), count)| Row {
                day,
                project,
                signal,
                count,
            })
            .collect();
        rows.sort_by_key(|r| (r.day, r.project, r.signal as usize));
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
/// the day of `at`.
pub fn prune(c: &Connection, at_ms: i64) -> rusqlite::Result<()> {
    let today = time::day_of(at_ms.saturating_mul(1_000_000));
    if let Ok(cutoff) = today.checked_sub(KEEP_DAYS.days()) {
        c.execute(
            "DELETE FROM usage_daily WHERE day<=?1",
            [cutoff.to_string()],
        )?;
    }
    Ok(())
}

/// Sends the counts to the writer every `period`. A full queue keeps them
/// for the next tick. The task ends when the writer stops.
pub async fn flush_every(counter: Counter, sender: store::Sender, period: Duration) {
    let mut ticks = tokio::time::interval(period);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        ticks.tick().await;
        let rows = counter.take();
        if rows.is_empty() {
            continue;
        }
        let (done, _) = tokio::sync::oneshot::channel();
        match sender.try_send(Write {
            job: Job::Usage(rows),
            done,
        }) {
            Ok(()) => {}
            Err(TrySendError::Full(write)) => {
                if let Job::Usage(rows) = write.job {
                    counter.put_back(rows);
                }
            }
            Err(TrySendError::Disconnected(_)) => return,
        }
    }
}

/// Sends what is left and waits until the writer stored it, for shutdown.
pub async fn flush(counter: &Counter, sender: &store::Sender) {
    let rows = counter.take();
    if !rows.is_empty() {
        store::queue_and_wait(sender, Job::Usage(rows)).await;
    }
}

/// Counted records and bytes per project and signal since `from`, of one
/// project or of all.
pub fn since(
    c: &Connection,
    from: Date,
    project: Option<i64>,
) -> Result<HashMap<(i64, Signal), Count>> {
    let mut q = c.prepare(
        "SELECT project_id, signal, SUM(records), SUM(bytes) FROM usage_daily
         WHERE day>=?1 AND (?2 IS NULL OR project_id=?2) GROUP BY project_id, signal",
    )?;
    let rows = q.query_map((from.to_string(), project), |r| {
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

/// Log bytes per project counted on the days `from` to `to`, both included.
pub fn log_bytes(c: &Connection, from: Date, to: Date) -> Result<HashMap<i64, u64>> {
    let mut q = c.prepare(
        "SELECT project_id, SUM(bytes) FROM usage_daily
         WHERE signal='logs' AND day>=?1 AND day<=?2 GROUP BY project_id",
    )?;
    let rows = q.query_map((from.to_string(), to.to_string()), |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
    })?;
    let mut out = HashMap::new();
    for row in rows {
        let (project, bytes) = row?;
        out.insert(project, u64::try_from(bytes).unwrap_or(0));
    }
    Ok(out)
}

#[cfg(test)]
#[path = "usage/tests.rs"]
mod tests;
