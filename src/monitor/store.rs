//! Monitor rows. The tables are created by the migrations in `crate::store`.

use super::{
    Change, CheckUrl, Expect, HeartbeatToken, NewMonitor, Outcome, Reason, State, Target,
    check::{Checked, Http},
    checkin::MonitorSlug,
};
use crate::{
    alert::{self, Incident, Notice},
    event::Timestamp,
};
use anyhow::{Context as _, Result};
use rusqlite::{
    Connection, OptionalExtension, params,
    types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef},
};
use serde::Serialize;

pub const MAX_MONITORS: i64 = 1000;
/// A failed HTTP check is confirmed this soon instead of after `every`.
const CONFIRM_AFTER_MS: Timestamp = 10_000;

#[derive(Debug, PartialEq, Eq)]
pub struct Added {
    pub id: i64,
    /// The URL of an HTTP monitor, or the ping path of a heartbeat,
    /// `/api/heartbeat/<token>`.
    pub target: String,
}

/// Errs when a monitor of `project` already takes check-ins for `slug`.
pub(super) fn slug_free(c: &Connection, project: i64, slug: &str) -> Result<()> {
    let in_use: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM monitors WHERE project_id=?1 AND slug=?2)",
        params![project, slug],
        |r| r.get(0),
    )?;
    anyhow::ensure!(
        !in_use,
        "the project already has a monitor with slug {slug}"
    );
    Ok(())
}

/// Stores `m` for `project` in the `Pending` state.
pub fn add(c: &Connection, project: i64, m: &NewMonitor, now: Timestamp) -> Result<Added> {
    let secs = |d: std::time::Duration| d.as_secs().cast_signed();
    let (kind, url, expect, keyword, token, timeout, grace, target, slug) = match &m.target {
        Target::Http {
            url,
            expect,
            timeout,
        } => {
            let (expect, keyword) = expect_columns(expect);
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
                None,
            )
        }
        Target::Heartbeat { grace, slug } => {
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
                slug.as_ref().map(MonitorSlug::as_str),
            )
        }
    };
    if let Some(slug) = slug {
        slug_free(c, project, slug)?;
    }
    // Monitors made by check-ins have a cap of their own, so a sender with
    // the public DSN cannot use up the room the operator needs.
    let inserted = c.execute(
        "INSERT INTO monitors(project_id,name,kind,url,expect,keyword,heartbeat_token,every_s,timeout_s,grace_s,state,created_at,slug)
         SELECT ?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'pending',?11,?13 WHERE (SELECT count(*) FROM monitors WHERE source='cli') < ?12",
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
            MAX_MONITORS,
            slug
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

/// The columns `expect` and `keyword` of `e`.
pub(super) fn expect_columns(e: &Expect) -> (&'static str, Option<&str>) {
    match e {
        Expect::Status => ("status", None),
        Expect::Keyword(k) => ("keyword", Some(k.as_str())),
        Expect::NoKeyword(k) => ("no_keyword", Some(k.as_str())),
    }
}

/// The [`Expect`] in the columns `expect` and `keyword` of monitor `id`.
pub(super) fn stored_expect(
    id: i64,
    expect: Option<&str>,
    keyword: Option<String>,
) -> Result<Expect> {
    Ok(match (expect, keyword) {
        (Some("status"), _) => Expect::Status,
        (Some("keyword"), Some(k)) => Expect::Keyword(k),
        (Some("no_keyword"), Some(k)) => Expect::NoKeyword(k),
        (expect, _) => anyhow::bail!("HTTP monitor {id} has unknown expect {expect:?}"),
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
            "UPDATE monitors SET last_ping_at=?2,due_at=?2+(every_s+coalesce(grace_s,0))*1000 WHERE id=?1",
            params![monitor, at],
        )?;
    }
    Ok(found)
}

/// Opens an incident for each heartbeat that is not down and is past its
/// `due_at` at `at`, and deletes expired history within
/// [`super::retention::BUDGET`].
/// A run that outlived its maximum runtime times out, a monitor with a slug
/// missed a check-in, and any other heartbeat is late.
pub fn sweep(c: &Connection, at: Timestamp) -> Result<()> {
    let late: Vec<(i64, Option<i64>, bool)> = c
        .prepare(
            "SELECT id,CASE WHEN run_started_at+coalesce(max_runtime_s,?2)*1000<?1 THEN coalesce(max_runtime_s,?2) END,
             slug IS NOT NULL FROM monitors WHERE kind='heartbeat' AND state<>'down' AND due_at<?1",
        )?
        .query_map(params![at, super::checkin_store::default_max_runtime_s()], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    for (monitor, timed_out, slug) in late {
        let reason = match (timed_out, slug) {
            (Some(secs), _) => Reason::TimedOut(secs.cast_unsigned() / 60),
            (None, true) => Reason::Missed,
            (None, false) => Reason::Late,
        };
        record(c, monitor, at, Outcome::Down(reason), None, None)?;
        c.execute(
            "UPDATE monitors SET due_at=NULL,run_id=NULL,run_started_at=NULL WHERE id=?1",
            [monitor],
        )?;
    }
    // A down monitor is not swept above, so a run left open while it is down
    // would block new runs and config until a matching end arrived.
    c.execute(
        "UPDATE monitors SET run_id=NULL,run_started_at=NULL WHERE state='down'
         AND run_started_at+coalesce(max_runtime_s,?2)*1000<?1",
        params![at, super::checkin_store::default_max_runtime_s()],
    )?;
    super::retention::prune(c, at, super::retention::BUDGET)?;
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
        let expect = stored_expect(id, expect.as_deref(), keyword)?;
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
pub(super) fn record(
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
                    r.get::<_, State>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?
    else {
        return Ok(false);
    };
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
        params![monitor, next, at],
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
    /// The URL of an HTTP monitor, the ping path of a heartbeat, or
    /// `check-in <slug>` for a monitor made by check-ins.
    pub target: String,
    /// The slug that check-ins for this monitor carry.
    pub slug: Option<String>,
    /// `cli` for a monitor made by `davy monitor add`, `sdk` for one made by
    /// check-ins.
    pub source: String,
    pub config_changes: i64,
    /// What a heartbeat expects, `every 1h` or `cron 0 * * * *, UTC`.
    pub schedule: Option<String>,
    pub state: State,
    pub last_check_at: Option<Timestamp>,
    /// When the open incident started.
    pub open_since: Option<Timestamp>,
}

/// Every monitor, oldest first.
pub fn list(c: &Connection) -> Result<Vec<Listed>> {
    listed(c, None)
}

/// Monitor `id` as [`list`] shows it.
pub fn one(c: &Connection, id: i64) -> Result<Option<Listed>> {
    Ok(listed(c, Some(id))?.pop())
}

fn listed(c: &Connection, id: Option<i64>) -> Result<Vec<Listed>> {
    let mut q = c.prepare(
        "SELECT m.id,p.slug,m.name,coalesce(m.url,'/api/heartbeat/'||m.heartbeat_token,'check-in '||m.slug),m.state,m.last_check_at,
         (SELECT started_at FROM incidents i WHERE i.monitor_id=m.id AND i.resolved_at IS NULL),
         m.slug,m.source,m.kind,m.every_s,m.months,m.cron,m.zone,m.config_changes
         FROM monitors m JOIN projects p ON p.id=m.project_id WHERE ?1 IS NULL OR m.id=?1 ORDER BY m.id",
    )?;
    let rows = q.query_map([id], |r| {
        let schedule = (r.get::<_, String>(9)? == "heartbeat").then(|| {
            super::checkin_store::schedule_at(r, 10)
                .map_or_else(|_| "invalid stored schedule".to_owned(), |s| s.to_string())
        });
        Ok(Listed {
            id: r.get(0)?,
            project: r.get(1)?,
            name: r.get(2)?,
            target: r.get(3)?,
            slug: r.get(7)?,
            source: r.get(8)?,
            config_changes: r.get(14)?,
            schedule,
            state: r.get(4)?,
            last_check_at: r.get(5)?,
            open_since: r.get(6)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Deletes a monitor with its checks and incidents. False when there is none.
pub fn remove(c: &Connection, id: i64) -> Result<bool> {
    Ok(c.execute("DELETE FROM monitors WHERE id=?1", [id])? == 1)
}

impl ToSql for State {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(self.as_str().into())
    }
}

impl FromSql for State {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        let s = value.as_str()?;
        [Self::Pending, Self::Up, Self::Failing, Self::Down]
            .into_iter()
            .find(|state| state.as_str() == s)
            .ok_or_else(|| FromSqlError::Other(format!("unknown monitor state {s:?}").into()))
    }
}
