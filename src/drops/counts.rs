//! The lines each rule dropped, or as a shadow rule would have dropped,
//! per UTC day of receipt: the log writer counts them in memory, the usage
//! flusher brings them to `drop_daily` with the logs stored, and the sweep
//! prunes them as it prunes `usage_daily`.

use crate::{
    time,
    usage::{self, Count},
};
use anyhow::Result;
use jiff::{ToSpan as _, civil::Date};
use rusqlite::{Connection, params};
use std::collections::HashMap;

/// A day of receipt, a rule, whether it was live, and its project.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Key {
    pub day: Date,
    pub rule: i64,
    pub live: bool,
    pub project: i64,
}

fn narrow(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

fn count(records: i64, bytes: i64) -> Count {
    let wide = |n: i64| u64::try_from(n).unwrap_or(0);
    Count {
        records: wide(records),
        bytes: wide(bytes),
    }
}

/// Adds `rows` to `drop_daily`. A row of a project that is gone is left out.
pub fn write(c: &Connection, rows: &[(Key, Count)]) -> rusqlite::Result<()> {
    let mut add = c.prepare_cached(
        "INSERT INTO drop_daily(day, rule_id, live, project_id, records, bytes)
         SELECT ?1, ?2, ?3, id, ?5, ?6 FROM projects WHERE id=?4
         ON CONFLICT(day, rule_id, live) DO UPDATE
         SET records=records+excluded.records, bytes=bytes+excluded.bytes",
    )?;
    for (k, n) in rows {
        add.execute(params![
            k.day.to_string(),
            k.rule,
            k.live,
            k.project,
            narrow(n.records),
            narrow(n.bytes)
        ])?;
    }
    Ok(())
}

/// Deletes the counts of days before the [`usage::KEEP_DAYS`] that end on
/// the day of `at_ms`, at most [`usage::PRUNE_ROWS`] of them, and the
/// agents' shadow rules that expired.
pub fn prune(c: &Connection, at_ms: i64) -> rusqlite::Result<()> {
    let today = time::day_of(at_ms.saturating_mul(1_000_000));
    if let Ok(cutoff) = today.checked_sub(usage::KEEP_DAYS.days()) {
        c.execute(
            "DELETE FROM drop_daily WHERE (day, rule_id, live) IN
             (SELECT day, rule_id, live FROM drop_daily WHERE day<=?1 LIMIT ?2)",
            params![cutoff.to_string(), usage::PRUNE_ROWS],
        )?;
    }
    c.execute("DELETE FROM drop_rules WHERE expires_at<=?1", [at_ms])?;
    Ok(())
}

/// Lines live rules dropped per project from the day `from` to the day
/// `to`, both included, of one project or of all: `filtered` in usage.
pub fn filtered_between(
    c: &Connection,
    from: Date,
    to: Date,
    project: Option<i64>,
) -> Result<HashMap<i64, Count>> {
    let mut q = c.prepare(
        "SELECT project_id, sum(records), sum(bytes) FROM drop_daily
         WHERE live=1 AND day>=?1 AND day<=?2 AND (?3 IS NULL OR project_id=?3)
         GROUP BY project_id",
    )?;
    let rows = q.query_map(params![from.to_string(), to.to_string(), project], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?,
        ))
    })?;
    let mut out = HashMap::new();
    for row in rows {
        let (project, records, bytes) = row?;
        out.insert(project, count(records, bytes));
    }
    Ok(out)
}

/// What one rule counted from the day `from` on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counted {
    /// Lines it dropped while live.
    pub dropped: Count,
    /// Lines it would have dropped while a shadow rule.
    pub would_drop: Count,
    /// The first day it counted a line on.
    pub first: Option<Date>,
}

/// What each rule of one project or of all counted from the day `from` on.
pub fn by_rule(c: &Connection, from: Date, project: Option<i64>) -> Result<HashMap<i64, Counted>> {
    let mut q = c.prepare(
        "SELECT rule_id, live, sum(records), sum(bytes), min(day) FROM drop_daily
         WHERE day>=?1 AND (?2 IS NULL OR project_id=?2) GROUP BY rule_id, live",
    )?;
    let rows = q.query_map(params![from.to_string(), project], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, bool>(1)?,
            count(row.get(2)?, row.get(3)?),
            row.get::<_, String>(4)?,
        ))
    })?;
    let mut out: HashMap<i64, Counted> = HashMap::new();
    for row in rows {
        let (rule, live, total, first) = row?;
        let counted = out.entry(rule).or_default();
        if live {
            counted.dropped = total;
        } else {
            counted.would_drop = total;
        }
        let first: Option<Date> = first.parse().ok();
        counted.first = [counted.first, first].into_iter().flatten().min();
    }
    Ok(out)
}
