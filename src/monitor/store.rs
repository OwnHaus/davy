//! Monitor rows. The tables are created by the migrations in `crate::store`.

use super::{Change, Expect, HeartbeatToken, NewMonitor, Outcome, Reason, State, Target};
use crate::event::Timestamp;
use anyhow::{Context as _, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

pub const MAX_MONITORS: i64 = 1000;

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
    let found = record(c, monitor, at, outcome)?;
    if found {
        c.execute(
            "UPDATE monitors SET last_ping_at=?2 WHERE id=?1",
            params![monitor, at],
        )?;
    }
    Ok(found)
}

/// Stores one check row, moves the monitor to its next state and opens or
/// closes its incident. False when the monitor is gone.
fn record(c: &Connection, monitor: i64, at: Timestamp, outcome: Outcome) -> Result<bool> {
    let Some(state) = c
        .query_row("SELECT state FROM monitors WHERE id=?1", [monitor], |r| {
            r.get::<_, String>(0)
        })
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
        "INSERT INTO checks(monitor_id,at,ok,reason) VALUES (?1,?2,?3,?4)",
        params![monitor, at, reason.is_none(), reason],
    )?;
    let (next, change) = state.next(outcome);
    c.execute(
        "UPDATE monitors SET state=?2,last_check_at=?3 WHERE id=?1",
        params![monitor, next.as_str(), at],
    )?;
    match change {
        Some(Change::Opened(reason)) => c.execute(
            "INSERT INTO incidents(monitor_id,started_at,reason) VALUES (?1,?2,?3)",
            params![monitor, at, reason.to_string()],
        )?,
        Some(Change::Resolved) => c.execute(
            "UPDATE incidents SET resolved_at=?2 WHERE monitor_id=?1 AND resolved_at IS NULL",
            params![monitor, at],
        )?,
        None => 0,
    };
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
