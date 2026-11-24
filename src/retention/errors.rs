//! Deletes the events and user feedback a project keeps no longer, and
//! keeps what was counted from them true.
//!
//! An event expires by `received_at`, the time Davy stored it, so a client
//! clock cannot keep an event or expire it early. Deleting one takes it out
//! of its issue's `event_count` and hourly counts, moves `first_seen` to the
//! oldest event left, and drops an environment or release tag once no event
//! that carries it is left. `last_seen` stays: it is the newest event, and
//! that is never older than one it keeps. An issue whose last event goes is
//! deleted with its counts and tags, and its user feedback stays, unlinked,
//! until it expires too. When the error comes back it is a new issue with a
//! new id, as ids are never used twice, so a link in an old alert cannot
//! lead to another issue. The resolved or ignored state goes with the old
//! issue, and a new-issue alert is sent. `releases` stays, because it says
//! when a release was first seen, which decides whether a resolved issue
//! reopens. Release health counts sessions, not events, and has a
//! retention of its own.

use super::{Days, turns};
use anyhow::Result;
use rusqlite::{Connection, ErrorCode, params};
use std::{
    collections::{BTreeMap, btree_map::Entry},
    time::Duration,
};

/// How long one sweep may spend deleting errors. The sweep runs on the
/// writer thread every 10 s, and ingest waits while it runs, so 100 ms costs
/// at most 1% of the writer's time. The monitors have a budget of their own,
/// see `crate::monitor::retention::BUDGET`.
pub const BUDGET: Duration = Duration::from_millis(100);
/// Events one statement deletes. Each issue a batch touches costs a few
/// more statements to keep its counts, about 0.3 ms when its pages are not
/// cached, so a batch of 200 events in 200 issues stays near 60 ms and a
/// sweep ends about that much past its budget (measured, `docs/design.md`).
const BATCH: i64 = 200;

/// The events of a project's own or the server's `default` retention
/// deleted first, then user feedback, as classes that take turns.
pub fn sweep(c: &Connection, at: i64, default: Days, budget: Duration) -> Result<()> {
    turns(2, budget, |class| match class {
        0 => events(c, at, default),
        _ => feedback(c, at, default),
    })
}

/// One expired event, as the delete returns it.
struct Gone {
    issue: i64,
    timestamp: i64,
    received_at: i64,
    environment: Option<String>,
    release: Option<String>,
}

/// What one batch took from one issue.
#[derive(Default)]
struct Taken {
    events: i64,
    /// Events per hour of `issue_hourly`.
    hours: BTreeMap<i64, i64>,
}

/// Deletes up to [`BATCH`] expired events, the oldest of each project
/// first, and fixes what counted them. Returns whether the batch was full.
fn events(c: &Connection, at: i64, default: Days) -> Result<bool> {
    // CROSS JOIN keeps projects as the outer loop, so each project's expired
    // events are one range of events_project_received.
    let mut q = c.prepare_cached(
        "DELETE FROM events WHERE id IN (SELECT e.id FROM projects p CROSS JOIN events e
         ON e.project_id=p.id AND e.received_at<?1-coalesce(p.error_retention_days,?2)*?3 LIMIT ?4)
         RETURNING issue_id, timestamp, received_at, environment, release",
    )?;
    let gone: Vec<Gone> = q
        .query_map(params![at, default.get(), super::DAY_MS, BATCH], |r| {
            Ok(Gone {
                issue: r.get(0)?,
                timestamp: r.get(1)?,
                received_at: r.get(2)?,
                environment: r.get(3)?,
                release: r.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let full = i64::try_from(gone.len()).is_ok_and(|n| n >= BATCH);
    let mut issues: BTreeMap<i64, Taken> = BTreeMap::new();
    let mut tags: BTreeMap<(&str, &str, i64), i64> = BTreeMap::new();
    for g in &gone {
        let taken = issues.entry(g.issue).or_default();
        taken.events += 1;
        *taken
            .hours
            .entry(g.timestamp.div_euclid(3_600_000) * 3_600_000)
            .or_default() += 1;
        let carried = [("environment", &g.environment), ("release", &g.release)];
        for (key, value) in carried {
            if let Some(value) = value {
                match tags.entry((key, value, g.issue)) {
                    Entry::Vacant(v) => drop(v.insert(g.received_at)),
                    Entry::Occupied(mut o) => *o.get_mut() = (*o.get()).max(g.received_at),
                }
            }
        }
    }
    for ((key, value, issue), newest) in tags {
        // last_at is when an event with this tag last arrived. When that is
        // no later than the newest one just deleted, none is left.
        c.prepare_cached(
            "DELETE FROM issue_tags WHERE key=?1 AND value=?2 AND issue_id=?3 AND last_at<=?4",
        )?
        .execute(params![key, value, issue, newest])?;
    }
    for (issue, taken) in issues {
        recount(c, issue, &taken)?;
    }
    Ok(full)
}

/// Takes what one batch deleted out of `issue`'s counts, or deletes the
/// issue when it has no event left.
fn recount(c: &Connection, issue: i64, taken: &Taken) -> Result<()> {
    for (hour, n) in &taken.hours {
        c.prepare_cached("UPDATE issue_hourly SET count=count-?3 WHERE issue_id=?1 AND hour=?2")?
            .execute(params![issue, hour, n])?;
    }
    c.prepare_cached("DELETE FROM issue_hourly WHERE issue_id=?1 AND count<=0")?
        .execute([issue])?;
    let oldest: Option<i64> = c
        .prepare_cached("SELECT timestamp FROM events WHERE issue_id=?1 ORDER BY id LIMIT 1")?
        .query_row([issue], |r| r.get(0))
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            e => Err(e),
        })?;
    match oldest {
        Some(first_seen) => {
            c.prepare_cached(
                "UPDATE issues SET event_count=max(event_count-?2,0), first_seen=?3 WHERE id=?1",
            )?
            .execute(params![issue, taken.events, first_seen])?;
        }
        None => forget(c, issue)?,
    }
    Ok(())
}

/// Deletes an issue that has no event left. A row in a table that still
/// points at the issue keeps it, with no events, rather than fail the
/// writer's batch, ingest included.
fn forget(c: &Connection, issue: i64) -> Result<()> {
    c.prepare_cached("UPDATE user_feedback SET issue_id=NULL WHERE issue_id=?1")?
        .execute([issue])?;
    c.prepare_cached("DELETE FROM issue_hourly WHERE issue_id=?1")?
        .execute([issue])?;
    c.prepare_cached("DELETE FROM issue_tags WHERE issue_id=?1")?
        .execute([issue])?;
    match c
        .prepare_cached("DELETE FROM issues WHERE id=?1")?
        .execute([issue])
    {
        Ok(_) => {
            c.prepare_cached(
                "INSERT INTO id_floor(name, id) VALUES ('issues', ?1)
                 ON CONFLICT DO UPDATE SET id=max(id, excluded.id)",
            )?
            .execute([issue])?;
            Ok(())
        }
        Err(rusqlite::Error::SqliteFailure(e, _)) if e.code == ErrorCode::ConstraintViolation => {
            c.prepare_cached("UPDATE issues SET event_count=0 WHERE id=?1")?
                .execute([issue])?;
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

/// Deletes up to [`BATCH`] expired user feedback. Returns whether the
/// batch was full.
fn feedback(c: &Connection, at: i64, default: Days) -> Result<bool> {
    let deleted = c
        .prepare_cached(
            "DELETE FROM user_feedback WHERE id IN (SELECT f.id FROM projects p CROSS JOIN user_feedback f
             ON f.project_id=p.id AND f.received_at<?1-coalesce(p.error_retention_days,?2)*?3 LIMIT ?4)",
        )?
        .execute(params![at, default.get(), super::DAY_MS, BATCH])?;
    Ok(i64::try_from(deleted).is_ok_and(|n| n >= BATCH))
}

#[cfg(test)]
#[path = "errors_tests.rs"]
mod tests;
