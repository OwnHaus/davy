//! Deletes the events and user feedback a project keeps no longer, and
//! keeps what was counted from them true.
//!
//! An event expires by `received_at`, the time Davy stored it, so a client
//! clock cannot keep an event or expire it early. Deleting one takes it out
//! of its issue's `event_count` and hourly counts, sets `first_seen` and
//! `last_seen` to the oldest and newest timestamp of the events left, and
//! drops an environment or release tag once no event that carries it is
//! left. An issue whose last event goes is deleted with its counts and
//! tags, and its user feedback stays, unlinked, until it expires too. An
//! issue with much feedback is unlinked from it over several batches, and
//! keeps an `event_count` of 0 until the last is unlinked. An error that
//! comes back by then starts that issue again, unresolved and with a
//! new-issue alert, under its old id. Later it is a new issue with a new
//! id, as ids are never used twice, so a link in an old alert cannot lead
//! to another issue. The resolved or ignored state goes with the old
//! issue, and a new-issue alert is sent. `releases` stays, because it says
//! when a release was first seen, which decides whether a resolved issue
//! reopens. Release health counts sessions, not events, and has a
//! retention of its own.

use super::{Days, turns};
use anyhow::Result;
use rusqlite::{Connection, ErrorCode, params};
use std::{
    collections::{BTreeMap, BTreeSet, HashSet, btree_map::Entry},
    time::Duration,
};

/// How long one sweep may spend deleting errors. The sweep runs on the
/// writer thread every 10 s, and ingest waits while it runs, so 100 ms costs
/// at most 1% of the writer's time. The monitors have a budget of their own,
/// see `crate::monitor::retention::BUDGET`.
pub const BUDGET: Duration = Duration::from_millis(100);
/// Events one batch deletes at most, and user feedback one statement
/// deletes at most.
const BATCH: i64 = 200;
/// User feedback one batch unlinks at most from issues whose last event
/// expired. SQLite rewrites a row whole when its `issue_id` changes, and a
/// row with every field at its limit is about 26 KB, so 50 rows are about
/// 320 pages, which stays within the work of one batch of events.
const UNLINK: i64 = 50;
/// The work one batch of events may do, in units of about one event
/// deleted, about 6 µs. An event costs 1, and 1 more per 4 KiB of payload,
/// user id, release and environment, which SQLite frees page by page. Each
/// of the last three is capped at ingest, but a row stored before its cap
/// keeps its size. The first event of an issue in a batch
/// costs [`ISSUE_WORK`] more, for the statements that keep the issue's
/// counts and tags. So a batch stops after about 10 ms of work, and a sweep
/// ends at most that much past its budget, however its events spread over
/// issues. The feedback of a deleted issue is unlinked in batches of its
/// own, see [`UNLINK`]. The costs come from the measurements in
/// `docs/design.md`.
const WORK: i64 = 1_600;
/// About 0.3 ms of page reads, what an issue a batch touches costs when its
/// pages are not cached.
const ISSUE_WORK: i64 = 50;
/// The payload bytes that cost one unit of [`WORK`].
const PAGE: i64 = 4_096;

/// The events of a project's own or the server's `default` retention
/// deleted first, then user feedback, then the feedback links of issues
/// with no event left, as classes that take turns.
pub fn sweep(c: &Connection, at: i64, default: Days, budget: Duration) -> Result<()> {
    turns(3, budget, |class| match class {
        0 => events(c, at, default),
        1 => feedback(c, at, default),
        _ => unlink(c),
    })
}

/// One expired event, as the delete returns it.
struct Gone {
    project: i64,
    issue: i64,
    timestamp: i64,
    received_at: i64,
    environment: Option<String>,
    release: Option<String>,
}

/// The expired events of one batch, see [`pick`], with what each weighs.
/// CROSS JOIN keeps projects as the outer loop, so each project's expired
/// events are one range of events_project_received, read in its order
/// without a sort. The order is what lets [`untag`] look for tied events
/// only.
const PICK: &str =
    "SELECT e.id, e.issue_id, coalesce(length(e.payload), 0) + coalesce(octet_length(e.user_id), 0)
         + coalesce(octet_length(e.release), 0) + coalesce(octet_length(e.environment), 0)
         FROM projects p CROSS JOIN events e
         ON e.project_id=p.id AND e.received_at<?1-coalesce(p.error_retention_days,?2)*?3
         ORDER BY p.id, e.received_at, e.id LIMIT ?4";
/// Drops a tag of an issue that no event left carries, see [`untag`].
const UNTAG: &str =
    "DELETE FROM issue_tags WHERE key=?1 AND value=?2 AND issue_id=?3 AND last_at<=?4
         AND NOT EXISTS(SELECT 1 FROM events WHERE project_id=?5 AND received_at=issue_tags.last_at
         AND issue_id=?3 AND CASE ?1 WHEN 'environment' THEN environment ELSE release END=?2)";
/// The oldest and newest timestamp of an issue's events. Two subqueries,
/// so each is one lookup in events_issue_timestamp.
const SEEN: &str = "SELECT (SELECT min(timestamp) FROM events WHERE issue_id=?1),
             (SELECT max(timestamp) FROM events WHERE issue_id=?1)";
/// The user feedback of issues with no event left. issues_emptied holds
/// only the issues with an event_count of 0, so finding them reads no
/// other issue.
const LINKED: &str =
    "SELECT f.id, f.issue_id FROM issues i CROSS JOIN user_feedback f ON f.issue_id=i.id
             WHERE i.event_count=0 AND NOT EXISTS(SELECT 1 FROM events WHERE issue_id=i.id)
             LIMIT ?1";

/// What one batch took from one issue.
#[derive(Default)]
struct Taken {
    events: i64,
    /// Events per hour of `issue_hourly`.
    hours: BTreeMap<i64, i64>,
}

/// Deletes one batch of expired events, see [`pick`], and fixes what
/// counted them. Returns whether more may wait.
fn events(c: &Connection, at: i64, default: Days) -> Result<bool> {
    let (ids, more) = pick(c, at, default)?;
    let mut delete = c.prepare_cached(
        "DELETE FROM events WHERE id=?1
         RETURNING project_id, issue_id, timestamp, received_at, environment, release",
    )?;
    let mut gone = Vec::with_capacity(ids.len());
    for id in ids {
        gone.push(delete.query_row([id], |r| {
            Ok(Gone {
                project: r.get(0)?,
                issue: r.get(1)?,
                timestamp: r.get(2)?,
                received_at: r.get(3)?,
                environment: r.get(4)?,
                release: r.get(5)?,
            })
        })?);
    }
    let mut issues: BTreeMap<i64, Taken> = BTreeMap::new();
    let mut tags: BTreeMap<(&str, &str, i64), (i64, i64)> = BTreeMap::new();
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
                    Entry::Vacant(v) => drop(v.insert((g.project, g.received_at))),
                    Entry::Occupied(mut o) => o.get_mut().1 = o.get().1.max(g.received_at),
                }
            }
        }
    }
    for ((key, value, issue), (project, newest)) in tags {
        untag(c, key, value, issue, project, newest)?;
    }
    for (issue, taken) in issues {
        recount(c, issue, &taken)?;
    }
    Ok(more)
}

/// The expired events one batch deletes: the oldest of each project
/// first, up to [`BATCH`] events and about [`WORK`] of work, and at least
/// one. Returns their ids, and whether more may wait.
fn pick(c: &Connection, at: i64, default: Days) -> Result<(Vec<i64>, bool)> {
    let mut q = c.prepare_cached(PICK)?;
    let rows = q.query_map(params![at, default.get(), super::DAY_MS, BATCH], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?,
        ))
    })?;
    let mut ids = Vec::new();
    let mut issues = HashSet::new();
    let mut work = 0;
    for row in rows {
        let (id, issue, bytes) = row?;
        let first = !issues.contains(&issue);
        let cost = 1 + bytes / PAGE + if first { ISSUE_WORK } else { 0 };
        if !ids.is_empty() && work + cost > WORK {
            return Ok((ids, true));
        }
        issues.insert(issue);
        work += cost;
        ids.push(id);
    }
    let full = i64::try_from(ids.len()).is_ok_and(|n| n >= BATCH);
    Ok((ids, full))
}

/// Drops the tag `key=value` of `issue` when no event that carries it is
/// left. `newest` is when the newest event just deleted with it arrived.
fn untag(
    c: &Connection,
    key: &str,
    value: &str,
    issue: i64,
    project: i64,
    newest: i64,
) -> Result<()> {
    // last_at is when an event with this tag last arrived. Events expire
    // in the order they arrived, so an event left that carries the tag
    // arrived after the newest one deleted, which makes last_at later, or
    // at the same millisecond, which the index by project and time finds.
    c.prepare_cached(UNTAG)?
        .execute(params![key, value, issue, newest, project])?;
    Ok(())
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
    let seen: Option<(i64, i64)> = c
        .prepare_cached(SEEN)?
        .query_row([issue], |r| Ok(r.get::<_, Option<i64>>(0)?.zip(r.get(1)?)))?;
    match seen {
        Some((first_seen, last_seen)) => {
            c.prepare_cached(
                "UPDATE issues SET event_count=max(event_count-?2,0), first_seen=?3, last_seen=?4
                 WHERE id=?1",
            )?
            .execute(params![issue, taken.events, first_seen, last_seen])?;
        }
        None => forget(c, issue)?,
    }
    Ok(())
}

/// Deletes an issue that has no event left. While user feedback points at
/// the issue it stays, with an `event_count` of 0, until [`unlink`] has
/// unlinked the last or [`feedback`] has deleted it. A row of another table that points at it keeps it
/// the same way rather than fail the writer's batch, ingest included.
fn forget(c: &Connection, issue: i64) -> Result<()> {
    c.prepare_cached("DELETE FROM issue_hourly WHERE issue_id=?1")?
        .execute([issue])?;
    c.prepare_cached("DELETE FROM issue_tags WHERE issue_id=?1")?
        .execute([issue])?;
    let linked: bool = c
        .prepare_cached("SELECT EXISTS(SELECT 1 FROM user_feedback WHERE issue_id=?1)")?
        .query_row([issue], |r| r.get(0))?;
    let kept = linked
        || match c
            .prepare_cached("DELETE FROM issues WHERE id=?1")?
            .execute([issue])
        {
            Ok(_) => {
                c.prepare_cached(
                    "INSERT INTO id_floor(name, id) VALUES ('issues', ?1)
                     ON CONFLICT DO UPDATE SET id=max(id, excluded.id)",
                )?
                .execute([issue])?;
                false
            }
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == ErrorCode::ConstraintViolation =>
            {
                true
            }
            Err(e) => return Err(e.into()),
        };
    if kept {
        c.prepare_cached("UPDATE issues SET event_count=0 WHERE id=?1")?
            .execute([issue])?;
    }
    Ok(())
}

/// Unlinks up to [`UNLINK`] user feedback from issues with no event left,
/// and deletes each such issue once none points at it. Returns whether the
/// batch was full.
fn unlink(c: &Connection) -> Result<bool> {
    let linked: Vec<(i64, i64)> = c
        .prepare_cached(LINKED)?
        .query_map([UNLINK], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut clear = c.prepare_cached("UPDATE user_feedback SET issue_id=NULL WHERE id=?1")?;
    let mut issues = BTreeSet::new();
    for (feedback, issue) in &linked {
        clear.execute([feedback])?;
        issues.insert(*issue);
    }
    for issue in issues {
        forget(c, issue)?;
    }
    Ok(i64::try_from(linked.len()).is_ok_and(|n| n >= UNLINK))
}

/// Deletes up to [`BATCH`] expired user feedback, and each issue with no
/// event left that the feedback was the last to point at. Returns whether
/// the batch was full.
fn feedback(c: &Connection, at: i64, default: Days) -> Result<bool> {
    let linked: Vec<Option<i64>> = c
        .prepare_cached(
            "DELETE FROM user_feedback WHERE id IN (SELECT f.id FROM projects p CROSS JOIN user_feedback f
             ON f.project_id=p.id AND f.received_at<?1-coalesce(p.error_retention_days,?2)*?3 LIMIT ?4)
             RETURNING issue_id",
        )?
        .query_map(params![at, default.get(), super::DAY_MS, BATCH], |r| {
            r.get(0)
        })?
        .collect::<rusqlite::Result<_>>()?;
    let full = i64::try_from(linked.len()).is_ok_and(|n| n >= BATCH);
    // An issue whose last event expired in this sweep waits for its
    // feedback, and `unlink` finds an issue only through feedback left.
    let mut emptied = c.prepare_cached(
        "SELECT EXISTS(SELECT 1 FROM issues WHERE id=?1 AND event_count=0
         AND NOT EXISTS(SELECT 1 FROM events WHERE issue_id=?1))",
    )?;
    for issue in linked.into_iter().flatten().collect::<BTreeSet<i64>>() {
        if emptied.query_row([issue], |r| r.get(0))? {
            forget(c, issue)?;
        }
    }
    Ok(full)
}

#[cfg(test)]
#[path = "errors_tests.rs"]
mod tests;
