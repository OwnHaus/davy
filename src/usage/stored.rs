//! What Davy stores, counted where it cannot be rebuilt later: raw bytes of
//! the logs per index day and project with the pairs retention cut, and
//! SQLite's size once a day (`docs/design.md`, "What is counted: stored
//! logs, cut pairs, SQLite size").

use super::{Count, KEEP_DAYS, PRUNE_ROWS};
use crate::{
    logs::store::log_days::{Flush, Mark},
    time,
};
use anyhow::Result;
use jiff::{ToSpan as _, civil::Date};
use rusqlite::{Connection, OptionalExtension};
use std::collections::HashMap;

pub const MIGRATION: &str = "CREATE TABLE log_days(day TEXT NOT NULL, project_id INTEGER NOT NULL REFERENCES projects(id), records INTEGER NOT NULL, bytes INTEGER NOT NULL, cut INTEGER NOT NULL DEFAULT 0 CHECK(cut IN (0, 1)), PRIMARY KEY(day, project_id)) WITHOUT ROWID;
CREATE TABLE store_days(day TEXT PRIMARY KEY, at INTEGER NOT NULL, bytes INTEGER NOT NULL, events INTEGER NOT NULL) WITHOUT ROWID;";
/// Undoes [`MIGRATION`], for tests that run the migrations again.
#[cfg(test)]
pub const UNDO: &str = "DROP TABLE log_days; DROP TABLE store_days;";

/// The `firsts` row of when counting started. `First` does not name it, so
/// the setup page does not show it.
const COUNTING_START: &str = "log_days";

fn narrow(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// Applies `flush` in its order: each segment's counts, then its marks. A
/// project deleted since it was counted is skipped.
pub fn write(c: &Connection, flush: &Flush) -> rusqlite::Result<()> {
    if let Some(at) = flush.start {
        c.execute(
            "INSERT OR IGNORE INTO firsts(what, at) VALUES (?1, ?2)",
            (COUNTING_START, at),
        )?;
    }
    let mut add = c.prepare_cached(
        "INSERT INTO log_days(day, project_id, records, bytes)
         SELECT ?1, id, ?3, ?4 FROM projects WHERE id=?2
         ON CONFLICT(day, project_id) DO UPDATE
         SET records=records+excluded.records, bytes=bytes+excluded.bytes",
    )?;
    for s in &flush.segments {
        for r in &s.rows {
            add.execute((
                r.day.to_string(),
                r.project,
                narrow(r.count.records),
                narrow(r.count.bytes),
            ))?;
        }
        for m in &s.marks {
            mark(c, m)?;
        }
    }
    Ok(())
}

/// Sets `cut` on the rows `m` names. A cut row stays cut.
fn mark(c: &Connection, m: &Mark) -> rusqlite::Result<usize> {
    let list = |ids: &[i64]| serde_json::Value::from(ids).to_string();
    match m {
        Mark::Only(day, ids) => c.execute(
            "UPDATE log_days SET cut=1 WHERE day=?1 AND cut=0
             AND project_id IN (SELECT value FROM json_each(?2))",
            (day.to_string(), list(ids)),
        ),
        Mark::AllBut(day, ids) => c.execute(
            "UPDATE log_days SET cut=1 WHERE day=?1 AND cut=0
             AND project_id NOT IN (SELECT value FROM json_each(?2))",
            (day.to_string(), list(ids)),
        ),
        Mark::Absent(present) => {
            let days: Vec<String> = present.iter().map(Date::to_string).collect();
            c.execute(
                "UPDATE log_days SET cut=1 WHERE cut=0
                 AND day NOT IN (SELECT value FROM json_each(?1))",
                [serde_json::Value::from(days).to_string()],
            )
        }
    }
}

/// Writes SQLite's live size and the events kept at `at_ms`, unless the
/// day of `at_ms` has its sample.
pub fn sample(c: &Connection, at_ms: i64) -> rusqlite::Result<()> {
    let day = time::day_of(at_ms.saturating_mul(1_000_000)).to_string();
    let sampled = c
        .query_row("SELECT 1 FROM store_days WHERE day=?1", [&day], |_| Ok(()))
        .optional()?;
    if sampled.is_some() {
        return Ok(());
    }
    let pragma = |name: &str| c.query_row(&format!("PRAGMA {name}"), [], |r| r.get::<_, i64>(0));
    let live = pragma("page_count")?.saturating_sub(pragma("freelist_count")?);
    let bytes = live.saturating_mul(pragma("page_size")?);
    // The index holds event_count, so the sum reads no row of issues.
    let events: i64 = c.query_row(
        "SELECT coalesce(sum(event_count), 0) FROM issues INDEXED BY issues_status_sorts",
        [],
        |r| r.get(0),
    )?;
    c.execute(
        "INSERT INTO store_days(day, at, bytes, events) VALUES (?1, ?2, ?3, ?4)",
        (day, at_ms, bytes, events),
    )?;
    Ok(())
}

/// Deletes the rows of `log_days` and `store_days` of days other than the
/// [`KEEP_DAYS`] that end on the day of `at_ms`, at most [`PRUNE_ROWS`] of
/// each, as [`super::prune`] does for `usage_daily`.
pub fn prune(c: &Connection, at_ms: i64) -> rusqlite::Result<()> {
    let today = time::day_of(at_ms.saturating_mul(1_000_000));
    if let Ok(cutoff) = today.checked_sub(KEEP_DAYS.days()) {
        let cutoff = cutoff.to_string();
        c.execute(
            "DELETE FROM log_days WHERE (day, project_id) IN
             (SELECT day, project_id FROM log_days WHERE day<=?1 LIMIT ?2)",
            (&cutoff, PRUNE_ROWS),
        )?;
        c.execute(
            "DELETE FROM store_days WHERE day IN
             (SELECT day FROM store_days WHERE day<=?1 LIMIT ?2)",
            (&cutoff, PRUNE_ROWS),
        )?;
    }
    Ok(())
}

/// Stored records and raw bytes per project on the index days from `from`
/// to `to`, both included, of one project or of all.
pub fn between(
    c: &Connection,
    from: Date,
    to: Date,
    project: Option<i64>,
) -> Result<HashMap<i64, Count>> {
    let mut q = c.prepare(
        "SELECT project_id, SUM(records), SUM(bytes) FROM log_days
         WHERE day>=?1 AND day<=?2 AND (?3 IS NULL OR project_id=?3)
         GROUP BY project_id",
    )?;
    let rows = q.query_map((from.to_string(), to.to_string(), project), |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?,
        ))
    })?;
    let wide = |n: i64| u64::try_from(n).unwrap_or(0);
    let mut out = HashMap::new();
    for row in rows {
        let (project, records, bytes) = row?;
        out.insert(
            project,
            Count {
                records: wide(records),
                bytes: wide(bytes),
            },
        );
    }
    Ok(out)
}

#[cfg(test)]
#[path = "stored_tests.rs"]
mod tests;
