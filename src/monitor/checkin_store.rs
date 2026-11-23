//! Check-ins from the Sentry SDKs, applied to heartbeat monitors in the
//! writer. The rules are in `docs/design.md`, "Cron check-ins".

use super::{
    Outcome, Reason,
    checkin::{
        CheckIn, CheckInId, Config, DEFAULT_MAX_RUNTIME, Dropped, Item, MonitorConfig, Status,
    },
    schedule::Schedule,
    store::record,
};
use crate::{
    event::Timestamp,
    items::client_report::{self, Discarded},
};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, Row, params};
use std::time::Duration;

/// Migration 12. The `CHECK` on `zone` holds a monitor to one schedule: an
/// interval in `every_s`, or calendar months or a crontab with a zone.
pub const MIGRATION: &str = "ALTER TABLE monitors ADD COLUMN slug TEXT;
ALTER TABLE monitors ADD COLUMN source TEXT NOT NULL DEFAULT 'cli' CHECK(source IN ('cli','sdk'));
ALTER TABLE monitors ADD COLUMN months INTEGER;
ALTER TABLE monitors ADD COLUMN cron TEXT;
ALTER TABLE monitors ADD COLUMN zone TEXT CHECK((zone IS NULL AND months IS NULL AND cron IS NULL AND every_s>0) OR (zone IS NOT NULL AND kind='heartbeat' AND every_s=0 AND (months IS NULL)<>(cron IS NULL)));
ALTER TABLE monitors ADD COLUMN max_runtime_s INTEGER;
ALTER TABLE monitors ADD COLUMN run_id TEXT;
ALTER TABLE monitors ADD COLUMN run_started_at INTEGER;
ALTER TABLE monitors ADD COLUMN ended_run_id TEXT;
ALTER TABLE monitors ADD COLUMN due_at INTEGER;
ALTER TABLE monitors ADD COLUMN config_changes INTEGER NOT NULL DEFAULT 0;
ALTER TABLE channels ADD COLUMN sdk_next_at INTEGER NOT NULL DEFAULT 0;
CREATE TABLE checkin_starts(monitor_id INTEGER NOT NULL REFERENCES monitors(id) ON DELETE CASCADE, at INTEGER NOT NULL);
CREATE INDEX checkin_starts_at ON checkin_starts(monitor_id,at);
CREATE INDEX incidents_resolved ON incidents(resolved_at) WHERE resolved_at IS NOT NULL;
CREATE INDEX notifications_sent ON notifications(sent_at) WHERE sent_at IS NOT NULL;
CREATE INDEX notifications_failed ON notifications(failed_at) WHERE failed_at IS NOT NULL;
CREATE UNIQUE INDEX monitors_slug ON monitors(project_id, slug) WHERE slug IS NOT NULL;
CREATE INDEX monitors_due ON monitors(due_at) WHERE due_at IS NOT NULL;
UPDATE monitors SET due_at=last_ping_at+(every_s+coalesce(grace_s,0))*1000 WHERE kind='heartbeat' AND last_ping_at IS NOT NULL;";

/// Reverts [`MIGRATION`], for tests that run the migrations again.
#[cfg(test)]
pub const UNDO: &str = "ALTER TABLE channels DROP COLUMN sdk_next_at; DROP TABLE checkin_starts; ALTER TABLE monitors DROP COLUMN config_changes; DROP INDEX incidents_resolved; DROP INDEX notifications_sent; DROP INDEX notifications_failed; DROP INDEX monitors_slug; DROP INDEX monitors_due;
ALTER TABLE monitors DROP COLUMN zone; ALTER TABLE monitors DROP COLUMN source;
ALTER TABLE monitors DROP COLUMN slug; ALTER TABLE monitors DROP COLUMN months;
ALTER TABLE monitors DROP COLUMN cron; ALTER TABLE monitors DROP COLUMN max_runtime_s;
ALTER TABLE monitors DROP COLUMN run_id; ALTER TABLE monitors DROP COLUMN run_started_at;
ALTER TABLE monitors DROP COLUMN ended_run_id; ALTER TABLE monitors DROP COLUMN due_at; ";

/// Monitors that check-ins may create in one project.
pub const MAX_SDK_MONITORS: i64 = 100;
/// `ok` and `error` check-ins one monitor takes per minute, Sentry's limit.
const RATE_PER_MINUTE: i64 = 6;

#[must_use]
pub fn default_max_runtime_s() -> i64 {
    secs(DEFAULT_MAX_RUNTIME)
}

fn secs(d: Duration) -> i64 {
    i64::try_from(d.as_secs()).unwrap_or(i64::MAX)
}

fn ms(d: Duration) -> i64 {
    i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
}

/// The schedule in the columns `every_s`, `months`, `cron` and `zone`,
/// starting at column `i` of `r`.
pub fn schedule_at(r: &Row<'_>, i: usize) -> rusqlite::Result<Schedule> {
    let (every_s, months, cron, zone): (i64, Option<i64>, Option<String>, Option<String>) =
        (r.get(i)?, r.get(i + 1)?, r.get(i + 2)?, r.get(i + 3)?);
    Schedule::from_columns(every_s, months, cron.as_deref(), zone.as_deref()).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            i,
            rusqlite::types::Type::Text,
            "invalid schedule".into(),
        )
    })
}

/// A monitor that takes check-ins, as the writer needs it.
struct Monitor {
    id: i64,
    sdk: bool,
    schedule: Schedule,
    margin: Duration,
    max_runtime: Duration,
    run_id: Option<String>,
    run_started_at: Option<Timestamp>,
}

/// Applies one check-in at `at`. A refused one is counted in
/// `client_reports` under the category `monitor`, never an error, because the
/// SDK gets `200` either way.
pub fn apply(c: &Connection, project: i64, at: Timestamp, item: &Item) -> Result<()> {
    let refused = match item {
        Ok(check_in) => take(c, project, at, check_in)?.err(),
        Err(why) => Some(*why),
    };
    if let Some(why) = refused {
        let discarded = Discarded {
            reason: why.as_str().to_owned(),
            category: "monitor".to_owned(),
            quantity: 1,
        };
        client_report::add(c, project, at, &[discarded])?;
    }
    Ok(())
}

fn take(
    c: &Connection,
    project: i64,
    at: Timestamp,
    check_in: &CheckIn,
) -> Result<Result<(), Dropped>> {
    let found = find(c, project, check_in.slug.as_str())?;
    if let Some(m) = &found {
        let recent: i64 = c.query_row(
            "SELECT (SELECT count(*) FROM checks WHERE monitor_id=?1 AND at>?2)+(SELECT count(*) FROM checkin_starts WHERE monitor_id=?1 AND at>?2)",
            params![m.id, at - 60_000],
            |r| r.get(0),
        )?;
        if recent >= RATE_PER_MINUTE {
            return Ok(Err(Dropped::Rate));
        }
    }
    if found.is_none()
        && c.query_row(
            "SELECT EXISTS(SELECT 1 FROM monitors WHERE project_id=?1 AND slug=?2)",
            params![project, check_in.slug.as_str()],
            |r| r.get::<_, bool>(0),
        )?
    {
        return Ok(Err(Dropped::BadConfig));
    }
    let monitor = match (found, &check_in.config) {
        (Some(m), Config::Valid(config)) if m.sdk => configure(c, m, config)?,
        (Some(m), _) => m,
        (None, Config::Valid(config)) => match create(c, project, at, check_in, config)? {
            Some(m) => m,
            None => return Ok(Err(Dropped::MonitorCap)),
        },
        (None, Config::Invalid) => return Ok(Err(Dropped::BadConfig)),
        (None, Config::Absent) => return Ok(Err(Dropped::UnknownMonitor)),
    };
    match check_in.status {
        Status::InProgress => start(c, &monitor, at, check_in)?,
        Status::Ok | Status::Error => finish(c, &monitor, at, check_in)?,
    }
    Ok(Ok(()))
}

fn find(c: &Connection, project: i64, slug: &str) -> Result<Option<Monitor>> {
    let found = c.query_row(
        "SELECT id,source,coalesce(grace_s,0),coalesce(max_runtime_s,?3),run_id,run_started_at,every_s,months,cron,zone
         FROM monitors WHERE project_id=?1 AND slug=?2 AND kind='heartbeat'",
        params![project, slug, default_max_runtime_s()],
        |r| {
            Ok(Monitor {
                id: r.get(0)?,
                sdk: r.get::<_, String>(1)? == "sdk",
                margin: Duration::from_secs(r.get::<_, i64>(2)?.cast_unsigned()),
                max_runtime: Duration::from_secs(r.get::<_, i64>(3)?.cast_unsigned()),
                run_id: r.get(4)?,
                run_started_at: r.get(5)?,
                schedule: schedule_at(r, 6)?,
            })
        },
    )
    .optional();
    match found {
        Err(rusqlite::Error::FromSqlConversionFailure(_, _, _)) => Ok(None),
        other => Ok(other?),
    }
}

fn create(
    c: &Connection,
    project: i64,
    at: Timestamp,
    check_in: &CheckIn,
    config: &MonitorConfig,
) -> Result<Option<Monitor>> {
    let (every_s, months, cron, zone) = config.schedule.columns();
    let slug = check_in.slug.as_str();
    let inserted = c.execute(
        "INSERT INTO monitors(project_id,name,kind,slug,source,every_s,months,cron,zone,grace_s,max_runtime_s,state,created_at)
         SELECT ?1,?2,'heartbeat',?2,'sdk',?3,?4,?5,?6,?7,?8,'pending',?9
         WHERE (SELECT count(*) FROM monitors WHERE project_id=?1 AND source='sdk') < ?10",
        params![project, slug, every_s, months, cron, zone, secs(config.margin), secs(config.max_runtime), at, MAX_SDK_MONITORS],
    )?;
    Ok((inserted == 1).then(|| Monitor {
        id: c.last_insert_rowid(),
        sdk: true,
        schedule: config.schedule.clone(),
        margin: config.margin,
        max_runtime: config.max_runtime,
        run_id: None,
        run_started_at: None,
    }))
}

/// A monitor made by check-ins takes the schedule of the latest config.
fn configure(c: &Connection, m: Monitor, config: &MonitorConfig) -> Result<Monitor> {
    if m.run_started_at.is_some() {
        return Ok(m);
    }
    let (every_s, months, cron, zone) = config.schedule.columns();
    c.execute(
        "UPDATE monitors SET config_changes=config_changes+
         (every_s IS NOT ?2 OR months IS NOT ?3 OR cron IS NOT ?4 OR zone IS NOT ?5 OR grace_s IS NOT ?6 OR max_runtime_s IS NOT ?7),
         every_s=?2,months=?3,cron=?4,zone=?5,grace_s=?6,max_runtime_s=?7 WHERE id=?1",
        params![m.id, every_s, months, cron, zone, secs(config.margin), secs(config.max_runtime)],
    )?;
    Ok(Monitor {
        schedule: config.schedule.clone(),
        margin: config.margin,
        max_runtime: config.max_runtime,
        ..m
    })
}

/// Starts a run, which must end within the maximum runtime. It records no
/// check, so it neither opens nor closes an incident. The SDKs send a run's
/// start and end at once, so for a short job the end can arrive first; a start
/// for a run that already ended is ignored, or it would time out.
fn start(c: &Connection, m: &Monitor, at: Timestamp, check_in: &CheckIn) -> Result<()> {
    c.execute(
        "INSERT INTO checkin_starts(monitor_id,at) VALUES(?1,?2)",
        params![m.id, at],
    )?;
    if !m.sdk || m.run_started_at.is_some() {
        return Ok(());
    }
    c.execute(
        "UPDATE monitors SET run_id=?2,run_started_at=?3,last_ping_at=?3,due_at=?4
         WHERE id=?1 AND (?2 IS NULL OR ended_run_id IS NOT ?2)",
        params![
            m.id,
            check_in.id.as_ref().map(CheckInId::as_str),
            at,
            at.saturating_add(ms(m.max_runtime))
        ],
    )?;
    Ok(())
}

/// Records an `ok` or `error` as a ping or failure ping. It ends the open run
/// when it names that run or no run at all.
fn finish(c: &Connection, m: &Monitor, at: Timestamp, check_in: &CheckIn) -> Result<()> {
    let ends_run = match (&check_in.id, &m.run_id) {
        (None, _) => true,
        (Some(id), Some(run)) => id.as_str() == run,
        (Some(_), None) => false,
    };
    let started = m.run_started_at.filter(|_| ends_run);
    let took = match (check_in.duration, started) {
        (Some(d), _) => Some(ms(d)),
        (None, Some(started)) => Some(at.saturating_sub(started).max(0)),
        (None, None) => None,
    };
    let outcome = match check_in.status {
        Status::Error => Outcome::Down(Reason::Failed),
        Status::Ok | Status::InProgress => Outcome::Up,
    };
    record(c, m.id, at, outcome, None, took)?;
    let scheduled = m.schedule.due(at, m.margin);
    let (run_id, run_started_at, due) = match m.run_started_at {
        Some(started) if !ends_run => {
            let deadline = started.saturating_add(ms(m.max_runtime));
            (
                m.run_id.as_deref(),
                Some(started),
                Some(scheduled.map_or(deadline, |s| s.min(deadline))),
            )
        }
        _ => (None, None, scheduled),
    };
    c.execute(
        "UPDATE monitors SET last_ping_at=?2,due_at=?3,run_id=?4,run_started_at=?5,ended_run_id=coalesce(?6,ended_run_id) WHERE id=?1",
        params![m.id, at, due, run_id, run_started_at, check_in.id.as_ref().map(CheckInId::as_str)],
    )?;
    Ok(())
}
