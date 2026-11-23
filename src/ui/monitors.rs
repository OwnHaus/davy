//! The Uptime page: every monitor with its last day of checks, and one
//! monitor with its uptime, incidents and newest checks.

use super::{Refusal, issues::Query, issues::Reads, json};
use crate::{
    event::Timestamp,
    issues::{HOURS, hours_from},
    monitor::{State as Health, checkin_store::schedule_at},
    time::{now_ms, rfc3339_ms},
};
use anyhow::Result;
use axum::{
    Router,
    extract::{Path, State},
    http::{StatusCode, Uri},
    response::Response,
    routing::get,
};
use rusqlite::{Connection, OptionalExtension, Row};
use serde::Serialize;
use std::{collections::HashMap, sync::Arc};

const HOUR_MS: Timestamp = 3_600_000;
const DAY_MS: Timestamp = 24 * HOUR_MS;
/// Incidents and checks on the page of one monitor.
pub const NEWEST: u32 = 50;

pub fn routes(reads: Arc<Reads>) -> Router {
    Router::new()
        .route("/ui/api/monitors", get(list))
        .route("/ui/api/monitors/{id}", get(detail))
        .with_state(reads)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum MonitorKind {
    Http,
    Heartbeat,
}

/// The checks of one hour.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct Hour {
    pub checks: u32,
    pub failed: u32,
    /// The mean response time of the checks that have one, in milliseconds.
    pub ms: Option<u32>,
    /// An incident was open during some of the hour. A failed check alone
    /// does not make an hour down, as it does not lower the uptime.
    pub down: bool,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct MonitorRow {
    pub id: i64,
    pub project: String,
    pub name: String,
    pub kind: MonitorKind,
    /// The scheme, host and path an HTTP monitor checks. The query and a
    /// login may hold a credential, so they stay out, and a heartbeat's ping
    /// URL is one, so it has none.
    pub url: Option<String>,
    pub state: Health,
    /// How often an HTTP monitor is checked, or the interval of a
    /// heartbeat; 0 for a heartbeat on a calendar schedule.
    pub every_s: i64,
    /// What a heartbeat expects, such as `every 1h` or
    /// `cron 0 * * * *, Europe/Prague`.
    pub schedule: Option<String>,
    /// The slug of a heartbeat that takes Sentry SDK check-ins.
    pub slug: Option<String>,
    /// When Davy last checked an HTTP monitor, or a heartbeat last pinged.
    pub last_heard: Option<String>,
    /// When the open incident started.
    pub open_since: Option<String>,
    /// Checks in each of the [`HOURS`] hours from `hours_from`.
    pub hourly: Vec<Hour>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct MonitorPage {
    pub monitors: Vec<MonitorRow>,
    /// The start of the first hour of every row's hourly chart.
    pub hours_from: String,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct Incident {
    pub id: i64,
    pub started: String,
    pub resolved: Option<String>,
    pub reason: String,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct Check {
    pub at: String,
    pub ok: bool,
    pub status: Option<u16>,
    pub ms: Option<u32>,
    pub reason: Option<String>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct MonitorDetail {
    pub monitor: MonitorRow,
    pub hours_from: String,
    /// The share of the last 7 days without an open incident, from 0 to 1,
    /// counted from when the monitor was added. None for a monitor added
    /// this millisecond.
    pub uptime_7d: Option<f64>,
    pub uptime_30d: Option<f64>,
    pub timeout_s: Option<i64>,
    pub grace_s: Option<i64>,
    pub last_ping: Option<String>,
    /// When a heartbeat that gets no ping opens an incident: after its
    /// interval and grace, at its next scheduled time plus margin, or at the
    /// end of the maximum runtime of an open run. None before its first ping.
    pub expected_by: Option<String>,
    /// The [`NEWEST`] newest incidents.
    pub incidents: Vec<Incident>,
    /// The [`NEWEST`] newest checks.
    pub checks: Vec<Check>,
}

async fn list(State(reads): State<Arc<Reads>>, uri: Uri) -> Result<Response, Refusal> {
    Query::parse(&uri, &[])?;
    json(&reads.run(|c| page(c, now_ms())).await?)
}

async fn detail(
    State(reads): State<Arc<Reads>>,
    Path(id): Path<String>,
    uri: Uri,
) -> Result<Response, Refusal> {
    Query::parse(&uri, &[])?;
    let not_found = || Refusal::new(StatusCode::NOT_FOUND, "no such monitor");
    let id: i64 = id.parse().map_err(|_not_a_number| not_found())?;
    reads
        .run(move |c| one(c, id, now_ms()))
        .await?
        .map_or_else(|| Err(not_found()), |d| json(&d))
}

const COLUMNS: &str = "SELECT m.id,p.slug,m.name,m.kind,m.url,m.state,m.every_s,m.last_check_at,
    (SELECT started_at FROM incidents i WHERE i.monitor_id=m.id AND i.resolved_at IS NULL),
    m.timeout_s,m.grace_s,m.last_ping_at,m.created_at,m.every_s,m.months,m.cron,m.zone,m.slug,m.due_at
    FROM monitors m JOIN projects p ON p.id=m.project_id";

/// A monitor row and the columns only its own page shows.
struct Stored {
    row: MonitorRow,
    timeout_s: Option<i64>,
    grace_s: Option<i64>,
    last_ping_at: Option<Timestamp>,
    created_at: Timestamp,
    due_at: Option<Timestamp>,
}

fn stored(r: &Row<'_>) -> rusqlite::Result<Stored> {
    let kind = match r.get_ref(3)?.as_str()? {
        "heartbeat" => MonitorKind::Heartbeat,
        _ => MonitorKind::Http,
    };
    let url: Option<String> = r.get(4)?;
    Ok(Stored {
        row: MonitorRow {
            id: r.get(0)?,
            project: r.get(1)?,
            name: r.get(2)?,
            kind,
            url: url.as_deref().and_then(shown_url),
            state: r.get(5)?,
            every_s: r.get(6)?,
            schedule: match kind {
                MonitorKind::Http => None,
                MonitorKind::Heartbeat => Some(schedule_at(r, 13)?.to_string()),
            },
            slug: r.get(17)?,
            last_heard: match kind {
                MonitorKind::Http => r.get::<_, Option<Timestamp>>(7)?,
                MonitorKind::Heartbeat => r.get::<_, Option<Timestamp>>(11)?,
            }
            .map(rfc3339_ms),
            open_since: r.get::<_, Option<Timestamp>>(8)?.map(rfc3339_ms),
            hourly: vec![Hour::default(); HOURS],
        },
        timeout_s: r.get(9)?,
        grace_s: r.get(10)?,
        last_ping_at: r.get(11)?,
        created_at: r.get(12)?,
        due_at: r.get(18)?,
    })
}

/// `url` without its login, query and fragment.
pub fn shown_url(url: &str) -> Option<String> {
    let mut u = url::Url::parse(url).ok()?;
    u.set_username("").ok()?;
    u.set_password(None).ok()?;
    u.set_query(None);
    u.set_fragment(None);
    Some(u.into())
}

/// Every monitor by project and name, with its checks per hour.
pub fn page(c: &Connection, now: Timestamp) -> Result<MonitorPage> {
    let from = hours_from(now);
    let mut rows: Vec<MonitorRow> = c
        .prepare(&format!("{COLUMNS} ORDER BY p.slug, m.name, m.id"))?
        .query_map([], stored)?
        .map(|s| s.map(|s| s.row))
        .collect::<rusqlite::Result<_>>()?;
    let mut hourly = hourly(c, None, from, now)?;
    for row in &mut rows {
        if let Some(cells) = hourly.remove(&row.id) {
            row.hourly = cells;
        }
    }
    Ok(MonitorPage {
        monitors: rows,
        hours_from: rfc3339_ms(from),
    })
}

/// The [`HOURS`] hours from `from` of one monitor or of all, by monitor.
/// A monitor with no check and no incident in them is not in the map.
fn hourly(
    c: &Connection,
    monitor: Option<i64>,
    from: Timestamp,
    now: Timestamp,
) -> Result<HashMap<i64, Vec<Hour>>> {
    let mut by = HashMap::new();
    for (m, hour, cell) in hours(c, monitor, from)? {
        place(cells(&mut by, m), hour, from, cell);
    }
    let mut q = c.prepare(
        "SELECT monitor_id, started_at, resolved_at FROM incidents
         WHERE (?2 IS NULL OR monitor_id=?2) AND (resolved_at IS NULL OR resolved_at>?1)",
    )?;
    let spans = q.query_map(rusqlite::params![from, monitor], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
    })?;
    for span in spans {
        let (m, started, resolved): (i64, Timestamp, Option<Timestamp>) = span?;
        let cells = cells(&mut by, m);
        // An incident resolved in the millisecond it opened still covers
        // that millisecond.
        let end = resolved.unwrap_or(now).min(now).max(started + 1);
        let mut at = started.max(from);
        while at < end {
            if let Some(cell) = usize::try_from((at - from) / HOUR_MS)
                .ok()
                .and_then(|i| cells.get_mut(i))
            {
                cell.down = true;
            }
            at = (at.div_euclid(HOUR_MS) + 1) * HOUR_MS;
        }
    }
    Ok(by)
}

fn cells(by: &mut HashMap<i64, Vec<Hour>>, monitor: i64) -> &mut Vec<Hour> {
    by.entry(monitor)
        .or_insert_with(|| vec![Hour::default(); HOURS])
}

/// The checks per hour since `from`, of one monitor or of all.
fn hours(
    c: &Connection,
    monitor: Option<i64>,
    from: Timestamp,
) -> Result<Vec<(i64, Timestamp, Hour)>> {
    let mut q = c.prepare(
        "SELECT monitor_id, at/3600000, count(*), sum(ok=0), avg(ms) FROM checks
         WHERE at>=?1 AND (?2 IS NULL OR monitor_id=?2) GROUP BY monitor_id, at/3600000",
    )?;
    let rows = q.query_map(rusqlite::params![from, monitor], |r| {
        let mean: Option<f64> = r.get(4)?;
        Ok((
            r.get(0)?,
            r.get::<_, Timestamp>(1)? * HOUR_MS,
            Hour {
                checks: r.get(2)?,
                failed: r.get(3)?,
                ms: mean.map(rounded),
                down: false,
            },
        ))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a mean of response times in milliseconds is small and not negative"
)]
fn rounded(ms: f64) -> u32 {
    ms.round().clamp(0.0, f64::from(u32::MAX)) as u32
}

fn place(cells: &mut [Hour], hour: Timestamp, from: Timestamp, cell: Hour) {
    let i = usize::try_from((hour - from) / HOUR_MS).ok();
    if let Some(slot) = i.and_then(|i| cells.get_mut(i)) {
        *slot = cell;
    }
}

/// Monitor `id` with its uptime, incidents and newest checks, or `None`
/// when there is none.
pub fn one(c: &Connection, id: i64, now: Timestamp) -> Result<Option<MonitorDetail>> {
    let Some(mut s) = c
        .query_row(&format!("{COLUMNS} WHERE m.id=?1"), [id], stored)
        .optional()?
    else {
        return Ok(None);
    };
    let from = hours_from(now);
    if let Some(cells) = hourly(c, Some(id), from, now)?.remove(&id) {
        s.row.hourly = cells;
    }
    let spans: Vec<(Timestamp, Option<Timestamp>)> = c
        .prepare(
            "SELECT started_at,resolved_at FROM incidents WHERE monitor_id=?1
             AND (resolved_at IS NULL OR resolved_at>?2)",
        )?
        .query_map(rusqlite::params![id, now - 30 * DAY_MS], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let uptime = |days: Timestamp| uptime(&spans, s.created_at.max(now - days * DAY_MS), now);
    let incidents = c
        .prepare(
            "SELECT id,started_at,resolved_at,reason FROM incidents WHERE monitor_id=?1
             ORDER BY started_at DESC, id DESC LIMIT ?2",
        )?
        .query_map(rusqlite::params![id, NEWEST], |r| {
            Ok(Incident {
                id: r.get(0)?,
                started: rfc3339_ms(r.get(1)?),
                resolved: r.get::<_, Option<Timestamp>>(2)?.map(rfc3339_ms),
                reason: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let checks = c
        .prepare(
            "SELECT at,ok,status,ms,reason FROM checks WHERE monitor_id=?1
             ORDER BY at DESC LIMIT ?2",
        )?
        .query_map(rusqlite::params![id, NEWEST], |r| {
            Ok(Check {
                at: rfc3339_ms(r.get(0)?),
                ok: r.get(1)?,
                status: r.get(2)?,
                ms: r.get(3)?,
                reason: r.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(Some(MonitorDetail {
        hours_from: rfc3339_ms(from),
        uptime_7d: uptime(7),
        uptime_30d: uptime(30),
        timeout_s: s.timeout_s,
        grace_s: s.grace_s,
        last_ping: s.last_ping_at.map(rfc3339_ms),
        expected_by: s.due_at.map(rfc3339_ms),
        incidents,
        checks,
        monitor: s.row,
    }))
}

/// The share of `start..now` outside every incident span, or `None` for an
/// empty window. Incidents of one monitor never overlap.
#[expect(
    clippy::cast_precision_loss,
    reason = "30 days of milliseconds fit an f64 exactly"
)]
pub fn uptime(
    spans: &[(Timestamp, Option<Timestamp>)],
    start: Timestamp,
    now: Timestamp,
) -> Option<f64> {
    if now <= start {
        return None;
    }
    let down: Timestamp = spans
        .iter()
        .map(|&(from, to)| (to.unwrap_or(now).min(now) - from.max(start)).max(0))
        .sum();
    Some(1.0 - down as f64 / (now - start) as f64)
}
