//! The log searches people and agents ran, which the savings view reads to
//! say when a pattern was last asked for and how old the data searches read
//! (`docs/design.md`, "Search history").
//!
//! A search is recorded fire and forget: a full writer queue drops it, so
//! reading logs never waits on the writer. The table keeps the newest
//! [`KEEP_ROWS`] searches of the last [`KEEP_DAYS`] days. Beside it,
//! `search_days` keeps per UTC day, and per project or for every project
//! (`project_id` 0), the searches and the oldest read, which no trim of
//! the table evicts, so a busy day cannot hide an old read.

use crate::{
    event::Timestamp,
    logs::store::LogQuery,
    store::{self, Job},
    time,
};
use rusqlite::{Connection, params};

/// The most searches the table keeps. Each insert trims the oldest past it
/// by a range of the primary key.
pub const KEEP_ROWS: i64 = 10_000;
/// The days the sweep keeps searches.
pub const KEEP_DAYS: i64 = 90;
/// A longer query text, in UTF-8 bytes, is recorded as NULL with the rest
/// of the search: a pattern's example is log text of at most a few KiB,
/// and a search history row should stay small.
pub const MAX_QUERY: usize = 1_000;

/// Who ran a search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum By {
    /// An agent over MCP, with `search_logs` or `count_logs`.
    Agent,
    /// A person on the logs page of the web UI.
    Ui,
}

impl By {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Ui => "ui",
        }
    }
}

/// One search, as its filters were.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Search {
    pub by: By,
    pub project: Option<i64>,
    /// The window, unix nanoseconds.
    pub since: i64,
    pub until: i64,
    /// The minimum level, as [`crate::logs::LogLevel`] numbers it.
    pub level: Option<u8>,
    pub pattern: Option<u64>,
    pub query: Option<String>,
}

impl Search {
    /// The search `q` runs, its query text `None` when blank or over
    /// [`MAX_QUERY`] bytes.
    pub fn of(by: By, q: &LogQuery) -> Self {
        let query = q
            .text
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty() && t.len() <= MAX_QUERY);
        Self {
            by,
            project: q.project,
            since: q.since,
            until: q.until,
            level: q.min_level.map(|l| l as u8),
            pattern: q.pattern,
            query: query.map(str::to_owned),
        }
    }

    /// How far back the search read when it ran at `at_ms`, in whole days
    /// rounded up, 0 for a window that starts later.
    pub fn read_days(&self, at_ms: Timestamp) -> i64 {
        let read = at_ms
            .saturating_mul(1_000_000)
            .saturating_sub(self.since)
            .max(0);
        read / time::DAY_NS + i64::from(read % time::DAY_NS > 0)
    }
}

/// Queues `q` for the writer, if it has room, and does not wait for it.
pub fn record(writes: &store::Sender, by: By, q: &LogQuery) {
    let job = Job::Search {
        at: time::now_ms(),
        search: Search::of(by, q),
    };
    // A full or stopped queue drops the search: it only informs.
    drop(store::queue(writes, job));
}

/// Stores `s`, run at `at_ms`, trims the table to [`KEEP_ROWS`], and adds
/// it to its day in `search_days`: one more search, the count saturating,
/// and the oldest read the larger.
pub fn add(c: &Connection, at_ms: Timestamp, s: &Search) -> rusqlite::Result<()> {
    c.prepare_cached(
        "INSERT INTO searches(at, by, project_id, since, until, level, pattern, query)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )?
    .execute(params![
        at_ms,
        s.by.as_str(),
        s.project,
        s.since,
        s.until,
        s.level,
        // SQLite holds signed integers: the bits of the id, read back alike.
        s.pattern.map(u64::cast_signed),
        s.query,
    ])?;
    c.prepare_cached("DELETE FROM searches WHERE id <= last_insert_rowid() - ?1")?
        .execute([KEEP_ROWS])?;
    c.prepare_cached(
        "INSERT INTO search_days(day, project_id, searches, oldest_read_days) VALUES (?1, ?2, 1, ?3)
         ON CONFLICT(day, project_id) DO UPDATE SET
           searches = searches + (searches < 9223372036854775807),
           oldest_read_days = max(oldest_read_days, excluded.oldest_read_days)",
    )?
    .execute(params![
        time::day_of(at_ms.saturating_mul(1_000_000)).to_string(),
        s.project.unwrap_or(0),
        s.read_days(at_ms),
    ])?;
    Ok(())
}

/// Deletes the searches run before the [`KEEP_DAYS`] days before `at_ms`,
/// at most [`KEEP_ROWS`] of them, which is all the table holds, and the
/// days of `search_days` before that day, at most [`KEEP_ROWS`] a sweep.
pub fn prune(c: &Connection, at_ms: Timestamp) -> rusqlite::Result<()> {
    let cutoff = at_ms
        .saturating_mul(1_000_000)
        .saturating_sub(KEEP_DAYS.saturating_mul(time::DAY_NS));
    c.prepare_cached(
        "DELETE FROM searches WHERE id IN (SELECT id FROM searches WHERE at < ?1 ORDER BY id LIMIT ?2)",
    )?
    .execute(params![cutoff / 1_000_000, KEEP_ROWS])?;
    c.prepare_cached(
        "DELETE FROM search_days WHERE (day, project_id) IN
         (SELECT day, project_id FROM search_days WHERE day < ?1 LIMIT ?2)",
    )?
    .execute(params![time::day_of(cutoff).to_string(), KEEP_ROWS])?;
    Ok(())
}

#[cfg(test)]
#[path = "searches_tests.rs"]
mod tests;
