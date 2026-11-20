//! Monitor rows. The tables are created by the migrations in `crate::store`.

use super::{
    Change, CheckUrl, Expect, HeartbeatToken, NewMonitor, Outcome, Reason, State, Target,
    check::{Checked, Http},
};
use crate::{
    alert::{self, Incident, Notice},
    event::Timestamp,
};
use anyhow::{Context as _, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

pub const MAX_MONITORS: i64 = 1000;
const KEEP_CHECKS_MS: Timestamp = 30 * 24 * 60 * 60 * 1000;
/// Small enough that a sweep holds the writer for milliseconds, large enough
/// to delete faster than 1,000 monitors checking every 30 s add rows.
const PRUNE_BATCH: i64 = 1000;
/// A failed HTTP check is confirmed this soon instead of after `every`.
const CONFIRM_AFTER_MS: Timestamp = 10_000;

#[derive(Debug, PartialEq, Eq)]
pub struct Added {
    pub id: i64,
    /// The URL of an HTTP monitor, or the ping path of a heartbeat,
    /// `/api/heartbeat/<token>`.
    pub target: String,
}

/// Stores `m` for `project` in the `Pending` state.
pub fn add(c: &Connection, project: i64, m: &NewMonitor, now: Timestamp) -> Result<Added> {
    let secs = |d: std::time::Duration| d.as_secs().cast_signed();
    let (kind, url, expect, keyword, token, timeout, grace, target) = match &m.target {
        Target::Http {
            url,
            expect,
            timeout,
        } => {
            let (expect, keyword) = match expect {
                Expect::Status => ("status", None),
                Expect::Keyword(k) => ("keyword", Some(k.as_str())),
                Expect::NoKeyword(k) => ("no_keyword", Some(k.as_str())),
            };
            let timeout = Some(secs(*timeout));
            let target = url.as_str().to_owned();
            (
                "http",
                Some(url.as_str()),
                Some(expect),
                keyword,
                None,
                timeout,
                None,
                target,
            )
        }
        Target::Heartbeat { grace } => {
            let token = HeartbeatToken::random();
            let target = format!("/api/heartbeat/{}", token.as_str());
            let grace = Some(secs(*grace));
            (
                "heartbeat",
                None,
                None,
                None,
                Some(token),
                None,
                grace,
                target,
            )
        }
    };
    let inserted = c.execute(
        "INSERT INTO monitors(project_id,name,kind,url,expect,keyword,heartbeat_token,every_s,timeout_s,grace_s,state,created_at)
         SELECT ?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'pending',?11 WHERE (SELECT count(*) FROM monitors) < ?12",
        params![
            project,
            m.name,
            kind,
            url,
            expect,
            keyword,
            token.as_ref().map(HeartbeatToken::as_str),
            secs(m.every),
            timeout,
            grace,
            now,
            MAX_MONITORS
        ],
    )?;
    anyhow::ensure!(
        inserted == 1,
        "a data directory holds at most {MAX_MONITORS} monitors"
    );
    Ok(Added {
        id: c.last_insert_rowid(),
        target,
    })
}

/// The monitor a heartbeat token belongs to.
pub fn by_token(c: &Connection, token: &HeartbeatToken) -> Result<Option<i64>> {
    Ok(c.query_row(
        "SELECT id FROM monitors WHERE heartbeat_token=?1",
        [token.as_str()],
        |r| r.get(0),
    )
    .optional()?)
}

/// Records a heartbeat ping, a failure ping when `failed`. False when the
/// monitor is gone.
pub fn ping(c: &Connection, monitor: i64, at: Timestamp, failed: bool) -> Result<bool> {
    let outcome = if failed {
        Outcome::Down(Reason::Failed)
    } else {
        Outcome::Up
    };
    let found = record(c, monitor, at, outcome, None, None)?;
    if found {
        c.execute(
            "UPDATE monitors SET last_ping_at=?2 WHERE id=?1",
            params![monitor, at],
        )?;
    }
    Ok(found)
}

/// Opens an incident for each heartbeat that is up and got no ping within
/// its interval plus grace before `at`, and deletes up to 1,000 check rows
/// older than 30 days.
pub fn sweep(c: &Connection, at: Timestamp) -> Result<()> {
    let late: Vec<i64> = c
        .prepare(
            "SELECT id FROM monitors WHERE kind='heartbeat' AND state='up'
             AND last_ping_at+(every_s+grace_s)*1000<?1",
        )?
        .query_map([at], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for monitor in late {
        record(c, monitor, at, Outcome::Down(Reason::Late), None, None)?;
    }
    // CROSS JOIN keeps monitors as the outer loop, so each monitor's old rows
    // are a range of checks_monitor_at instead of a scan of every check.
    c.execute(
        "DELETE FROM checks WHERE rowid IN (SELECT c.rowid FROM monitors m
         CROSS JOIN checks c ON c.monitor_id=m.id AND c.at<?1 LIMIT ?2)",
        params![at.saturating_sub(KEEP_CHECKS_MS), PRUNE_BATCH],
    )?;
    Ok(())
}

/// The HTTP monitors due for a check at `now`, the longest waiting first. A
/// monitor is due `every` after its last check, or 10 s after it while it is
/// `Failing`, and at once when it was never checked.
pub fn due(c: &Connection, now: Timestamp) -> Result<Vec<Http>> {
    let mut q = c.prepare(
        "SELECT id,url,expect,keyword,timeout_s FROM monitors WHERE kind='http' AND (last_check_at IS NULL
         OR last_check_at+CASE state WHEN 'failing' THEN ?2 ELSE every_s*1000 END<=?1)
         ORDER BY coalesce(last_check_at,0),id",
    )?;
    let rows = q.query_map(params![now, CONFIRM_AFTER_MS], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, Option<i64>>(4)?,
        ))
    })?;
    rows.map(|row| {
        let (id, url, expect, keyword, timeout) = row?;
        let url = url
            .as_deref()
            .map(CheckUrl::parse)
            .with_context(|| format!("HTTP monitor {id} has no URL"))?
            .map_err(|e| anyhow::anyhow!("HTTP monitor {id}: {e}"))?;
        let expect = match (expect.as_deref(), keyword) {
            (Some("status"), _) => Expect::Status,
            (Some("keyword"), Some(k)) => Expect::Keyword(k),
            (Some("no_keyword"), Some(k)) => Expect::NoKeyword(k),
            (expect, _) => anyhow::bail!("HTTP monitor {id} has unknown expect {expect:?}"),
        };
        let timeout = timeout
            .and_then(|s| u64::try_from(s).ok())
            .with_context(|| format!("HTTP monitor {id} has no timeout"))?;
        Ok(Http {
            id,
            url,
            expect,
            timeout: std::time::Duration::from_secs(timeout),
        })
    })
    .collect()
}

/// Stores the result of an HTTP check that started at `at`. False when the
/// monitor is gone.
pub fn checked(c: &Connection, monitor: i64, at: Timestamp, checked: &Checked) -> Result<bool> {
    record(
        c,
        monitor,
        at,
        checked.outcome.clone(),
        checked.status,
        Some(checked.ms),
    )
}

/// Stores one check row, moves the monitor to its next state and opens or
/// closes its incident with a notice. False when the monitor is gone.
fn record(
    c: &Connection,
    monitor: i64,
    at: Timestamp,
    outcome: Outcome,
    status: Option<u16>,
    ms: Option<i64>,
) -> Result<bool> {
    let Some((state, project, name)) = c
        .query_row(
            "SELECT state,project_id,name FROM monitors WHERE id=?1",
            [monitor],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?
    else {
        return Ok(false);
    };
    let state = State::parse(&state)
        .with_context(|| format!("monitor {monitor} has unknown state {state:?}"))?;
    let reason = match &outcome {
        Outcome::Up => None,
        Outcome::Down(reason) => Some(reason.to_string()),
    };
    c.execute(
        "INSERT INTO checks(monitor_id,at,ok,status,ms,reason) VALUES (?1,?2,?3,?4,?5,?6)",
        params![monitor, at, reason.is_none(), status, ms, reason],
    )?;
    let (next, change) = state.next(outcome);
    c.execute(
        "UPDATE monitors SET state=?2,last_check_at=?3 WHERE id=?1",
        params![monitor, next.as_str(), at],
    )?;
    let notice = match change {
        Some(Change::Opened(reason)) => {
            let reason = reason.to_string();
            c.execute(
                "INSERT INTO incidents(monitor_id,started_at,reason) VALUES (?1,?2,?3)",
                params![monitor, at, reason],
            )?;
            let id = c.last_insert_rowid();
            Some(Notice::MonitorDown(Incident {
                id,
                monitor,
                name,
                reason,
            }))
        }
        Some(Change::Resolved) => {
            let open = c
                .query_row(
                    "SELECT id,reason FROM incidents WHERE monitor_id=?1 AND resolved_at IS NULL",
                    [monitor],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            c.execute(
                "UPDATE incidents SET resolved_at=?2 WHERE monitor_id=?1 AND resolved_at IS NULL",
                params![monitor, at],
            )?;
            open.map(|(id, reason)| {
                Notice::MonitorUp(Incident {
                    id,
                    monitor,
                    name,
                    reason,
                })
            })
        }
        None => None,
    };
    if let Some(notice) = notice {
        alert::store::notify(c, project, &notice, at)?;
    }
    Ok(true)
}

#[derive(Debug, Serialize)]
pub struct Listed {
    pub id: i64,
    pub project: String,
    pub name: String,
    /// The URL of an HTTP monitor, or the ping path of a heartbeat.
    pub target: String,
    pub state: State,
    pub last_check_at: Option<Timestamp>,
    /// When the open incident started.
    pub open_since: Option<Timestamp>,
}

/// Every monitor, oldest first.
pub fn list(c: &Connection) -> Result<Vec<Listed>> {
    let mut q = c.prepare(
        "SELECT m.id,p.slug,m.name,coalesce(m.url,'/api/heartbeat/'||m.heartbeat_token),m.state,m.last_check_at,
         (SELECT started_at FROM incidents i WHERE i.monitor_id=m.id AND i.resolved_at IS NULL)
         FROM monitors m JOIN projects p ON p.id=m.project_id ORDER BY m.id",
    )?;
    let rows = q.query_map([], |r| {
        Ok((
            Listed {
                id: r.get(0)?,
                project: r.get(1)?,
                name: r.get(2)?,
                target: r.get(3)?,
                state: State::Pending,
                last_check_at: r.get(5)?,
                open_since: r.get(6)?,
            },
            r.get::<_, String>(4)?,
        ))
    })?;
    rows.map(|row| {
        let (listed, state) = row?;
        let state = State::parse(&state)
            .with_context(|| format!("monitor {} has unknown state {state:?}", listed.id))?;
        Ok(Listed { state, ..listed })
    })
    .collect()
}

/// Deletes a monitor with its checks and incidents. False when there is none.
pub fn remove(c: &Connection, id: i64) -> Result<bool> {
    Ok(c.execute("DELETE FROM monitors WHERE id=?1", [id])? == 1)
}
