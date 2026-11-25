//! Discarded issues (`docs/design.md`, "Discarding an issue"): a person
//! marks an issue with `davy issue discard`, and the writer counts each new
//! event of it per day instead of storing it. Only the CLI writes the mark:
//! no MCP tool, no route and no ingest path sets `issues.discarded_at`,
//! because an agent's prompt and an event's payload can carry anyone's text.

use crate::{store, time, usage};
use anyhow::{Result, bail};
use jiff::{ToSpan as _, civil::Date};
use rusqlite::{Connection, OptionalExtension as _, TransactionBehavior, params};
use serde::Serialize;
use std::{collections::HashMap, io::Write, path::PathBuf};

/// Discarded events counted per UTC day of receipt and project.
const COUNT: &str = "INSERT INTO discarded_daily(day, project_id, records) VALUES (?1, ?2, 1)
     ON CONFLICT(day, project_id) DO UPDATE SET records=records+1";

/// Whether the issue an event groups into is discarded. A new error has
/// no issue yet, so it is not.
pub fn discarded(
    c: &Connection,
    project: i64,
    version: impl rusqlite::ToSql,
    fingerprint: &str,
) -> rusqlite::Result<bool> {
    Ok(c.prepare_cached(
        "SELECT discarded_at IS NOT NULL FROM issues
         WHERE project_id=?1 AND grouping_version=?2 AND fingerprint=?3",
    )?
    .query_row(params![project, version, fingerprint], |r| r.get(0))
    .optional()?
    .unwrap_or(false))
}

/// Counts one discarded event received at `received_ms`.
pub fn count(c: &Connection, project: i64, received_ms: i64) -> rusqlite::Result<()> {
    let day = time::day_of(received_ms.saturating_mul(1_000_000));
    c.prepare_cached(COUNT)?
        .execute(params![day.to_string(), project])?;
    Ok(())
}

/// Discarded events per project from the day `from` to the day `to`, both
/// included, of one project or of all.
pub fn between(
    c: &Connection,
    from: Date,
    to: Date,
    project: Option<i64>,
) -> Result<HashMap<i64, u64>> {
    let mut q = c.prepare(
        "SELECT project_id, SUM(records) FROM discarded_daily
         WHERE day>=?1 AND day<=?2 AND (?3 IS NULL OR project_id=?3) GROUP BY project_id",
    )?;
    let rows = q.query_map((from.to_string(), to.to_string(), project), |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
    })?;
    let mut out = HashMap::new();
    for row in rows {
        let (project, records) = row?;
        out.insert(project, u64::try_from(records).unwrap_or(0));
    }
    Ok(out)
}

/// Deletes the counts of days before the [`usage::KEEP_DAYS`] that end on
/// the day of `at_ms`, at most [`usage::PRUNE_ROWS`] of them.
pub fn prune(c: &Connection, at_ms: i64) -> rusqlite::Result<()> {
    let today = time::day_of(at_ms.saturating_mul(1_000_000));
    if let Ok(cutoff) = today.checked_sub(usage::KEEP_DAYS.days()) {
        c.execute(
            "DELETE FROM discarded_daily WHERE (day, project_id) IN
             (SELECT day, project_id FROM discarded_daily WHERE day<=?1 LIMIT ?2)",
            (cutoff.to_string(), usage::PRUNE_ROWS),
        )?;
    }
    Ok(())
}

/// What `davy issue discard` prints.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Discarded {
    pub issue: i64,
    pub project: String,
    pub title: String,
    pub discarded: bool,
    /// When the issue was discarded, RFC 3339, null after `--undo`.
    pub discarded_at: Option<String>,
}

/// Discards issue `id` at `now_ms`, or with `undo` stores its events again.
/// Discarding twice keeps the first time. An issue left with no event and
/// no feedback after `undo` is forgotten as retention forgets it, so a later
/// event opens a new issue.
pub fn set(c: &mut Connection, id: i64, undo: bool, now_ms: i64) -> Result<Discarded> {
    // IMMEDIATE, so no event of the issue lands between the mark and the
    // check for events.
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let changed = if undo {
        tx.execute("UPDATE issues SET discarded_at=NULL WHERE id=?1", [id])?
    } else {
        tx.execute(
            "UPDATE issues SET discarded_at=coalesce(discarded_at, ?2) WHERE id=?1",
            params![id, now_ms],
        )?
    };
    if changed == 0 {
        bail!("no issue {id}");
    }
    let (project, title, at, empty): (String, String, Option<i64>, bool) = tx.query_row(
        "SELECT p.slug, i.title, i.discarded_at, NOT EXISTS(SELECT 1 FROM events WHERE issue_id=i.id)
         FROM issues i JOIN projects p ON p.id=i.project_id WHERE i.id=?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    if undo && empty {
        crate::retention::errors::forget(&tx, id)?;
    }
    tx.commit()?;
    Ok(Discarded {
        issue: id,
        project,
        title,
        discarded: at.is_some(),
        discarded_at: at.map(time::rfc3339_ms),
    })
}

/// `davy issue`.
#[derive(clap::Subcommand)]
pub enum Issue {
    /// Stops storing new events of an issue: each one is counted per day,
    /// `davy usage` shows the count, and the event is dropped. The events
    /// stored before stay. `davy issues --discarded` lists the discarded
    /// issues. Prints the issue as JSON.
    Discard {
        /// The ID that `davy issues` prints.
        issue_id: i64,
        #[arg(long)]
        data: PathBuf,
        /// Store the issue's new events again. An issue left with no event
        /// and no feedback is forgotten, as retention forgets it.
        #[arg(long)]
        undo: bool,
    },
}

pub fn run(out: &mut impl Write, command: Issue) -> Result<()> {
    match command {
        Issue::Discard {
            issue_id,
            data,
            undo,
        } => {
            let done = set(&mut store::open(&data)?, issue_id, undo, time::now_ms())?;
            writeln!(out, "{}", crate::text::json(&done, false)?)?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "discard_tests.rs"]
mod tests;
