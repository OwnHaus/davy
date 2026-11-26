//! Daily quotas and spike protection per project (`docs/design.md`,
//! "Quotas and spike protection"). A person sets them with `davy project
//! quota`. Ingest checks them in memory against today's counts, which the
//! usage counter keeps as it counts what ingest accepts, so the hot path
//! never reads SQLite. What ingest refuses is counted in `client_reports`
//! under [`QUOTA`] or [`SPIKE`], in items, and a log request refused
//! before its body was read in `refused_requests`, in requests, as is a
//! `429` of a full body budget or queue under [`BUSY`].

use crate::{
    alert,
    items::client_report,
    store::{self, Job, Refused},
    time,
    usage::Signal,
};
use anyhow::Result;
use rusqlite::Connection;
use std::time::Duration;

pub mod cli;
mod gate;
pub mod settings;
mod spike;
pub mod status;
pub use gate::{Alert, Crossed, Flush, Gate, Limits, Refusal, Requests};
pub use settings::{DEFAULT_SPIKE, Settings, Spike};
pub use spike::BURST_SECS;

/// The client report reason of items refused past a daily quota.
pub const QUOTA: &str = "davy_quota";
/// The client report reason of items, or of log requests refused unread,
/// past a spike rate.
pub const SPIKE: &str = "davy_spike";
/// The reason of requests refused with `429` because the body budget or a
/// queue was full, counted in `refused_requests` under the project their
/// key picked, or [`NO_PROJECT`] when the key was not read yet.
pub const BUSY: &str = "davy_busy";
/// The project of a busy request whose key was not read yet.
pub const NO_PROJECT: i64 = 0;
/// How soon a change from `davy project quota` reaches ingest.
pub const RELOAD_EVERY: Duration = Duration::from_secs(5);
/// Days a `quota_alerts` row is kept, longer than any restart takes.
const ALERT_DAYS: i64 = 7;

/// Settings per project, the table that marks each alert sent, so a
/// quota warns once per project, signal, UTC day and quota across
/// restarts, and the count of requests refused unread or busy, per UTC
/// day (unix ms at midnight) like `client_reports`. Its `project_id` has
/// no reference, as a busy request may have no project.
pub const MIGRATION: &str = "ALTER TABLE projects ADD COLUMN quota_errors INTEGER CHECK(quota_errors > 0);
ALTER TABLE projects ADD COLUMN quota_logs INTEGER CHECK(quota_logs > 0);
ALTER TABLE projects ADD COLUMN quota_warn INTEGER NOT NULL DEFAULT 80 CHECK(quota_warn BETWEEN 1 AND 100);
ALTER TABLE projects ADD COLUMN spike INTEGER CHECK(spike >= 0);
ALTER TABLE projects ADD COLUMN spike_logs INTEGER CHECK(spike_logs >= 0);
CREATE TABLE quota_alerts(day TEXT NOT NULL, project_id INTEGER NOT NULL REFERENCES projects(id), signal TEXT NOT NULL, kind TEXT NOT NULL, quota INTEGER NOT NULL, PRIMARY KEY(day, project_id, signal, kind, quota)) WITHOUT ROWID;
CREATE TABLE refused_requests(day INTEGER NOT NULL, project_id INTEGER NOT NULL, reason TEXT NOT NULL, quantity INTEGER NOT NULL, PRIMARY KEY(day, project_id, reason)) WITHOUT ROWID;";
/// Undoes [`MIGRATION`], for tests that run the migrations again.
pub const UNDO: &str = "DROP TABLE refused_requests; DROP TABLE quota_alerts; ALTER TABLE projects DROP COLUMN spike_logs; ALTER TABLE projects DROP COLUMN spike; ALTER TABLE projects DROP COLUMN quota_warn; ALTER TABLE projects DROP COLUMN quota_logs; ALTER TABLE projects DROP COLUMN quota_errors;";

/// The place of a signal a quota can cap: error events, counted in
/// events, and logs, counted in bytes as received.
pub fn capped(signal: Signal) -> Option<usize> {
    match signal {
        Signal::Errors => Some(0),
        Signal::Logs => Some(1),
        Signal::Sessions | Signal::Feedback | Signal::Checkins => None,
    }
}

/// The signals a quota can cap, in the order of [`capped`].
pub const CAPPED: [Signal; 2] = [Signal::Errors, Signal::Logs];

/// The name of a capped signal in `quota_alerts`, alerts and status.
pub fn name(signal: Signal) -> &'static str {
    match signal {
        Signal::Errors => "errors",
        Signal::Logs => "logs",
        Signal::Sessions => "sessions",
        Signal::Feedback => "feedback",
        Signal::Checkins => "checkins",
    }
}

/// The Sentry rate limit category of a capped signal.
pub fn category(signal: Signal) -> &'static str {
    match signal {
        Signal::Errors => "error",
        Signal::Logs => "log_item",
        Signal::Sessions => "session",
        Signal::Feedback => "feedback",
        Signal::Checkins => "monitor",
    }
}

/// Loads every project's settings into `gate` and adds today's counts
/// from `usage_daily`, so a restart does not give a project its quota
/// again. Run it before the server takes requests.
pub fn start(c: &Connection, gate: &Gate, now_ns: i64) -> Result<()> {
    let today = time::day_of(now_ns);
    gate.sent(today, &sent(c, today)?, now_ns);
    gate.load(settings::all(c)?, now_ns);
    let counted = crate::usage::between(c, today, today, None)?;
    gate.restore(today, &counted, now_ns);
    Ok(())
}

/// Reads the settings every `period`, so a change a person made at the
/// CLI, in another process, reaches ingest.
pub async fn reload_every(c: Connection, gate: std::sync::Arc<Gate>, period: Duration) {
    let mut ticks = tokio::time::interval(period);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        ticks.tick().await;
        match settings::all(&c) {
            Ok(all) => gate.load(all, time::now_ns()),
            Err(e) => crate::self_report::warn("quotas", format_args!("not read: {e:#}")),
        }
    }
}

/// Hands what the gate refused and the quotas crossed to the writer.
/// What the writer did not commit goes back for the next flush.
pub async fn send(gate: &Gate, sender: &store::Sender) -> Result<(), Refused> {
    let flush = gate.take();
    if flush.is_empty() {
        return Ok(());
    }
    if let Err(e) = store::submit(sender, Job::Quota(flush.clone())).await {
        gate.put_back(flush);
        return Err(e);
    }
    Ok(())
}

/// The alerts of `day` already sent, with the quota each was sent for.
fn sent(c: &Connection, day: jiff::civil::Date) -> Result<Vec<(i64, Signal, Crossed, u64)>> {
    let mut q =
        c.prepare("SELECT project_id, signal, kind, quota FROM quota_alerts WHERE day=?1")?;
    let rows = q.query_map([day.to_string()], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, i64>(3)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (project, signal, kind, quota) = row?;
        let quota = u64::try_from(quota).unwrap_or(0);
        let signal = CAPPED.into_iter().find(|s| name(*s) == signal);
        if let (Some(signal), Some(crossed)) = (signal, Crossed::of(&kind)) {
            out.push((project, signal, crossed, quota));
        }
    }
    Ok(out)
}

/// Counts the refusals in `client_reports`, Davy's own reasons are few,
/// so they skip the cap on pairs a day an SDK's reports have, and the
/// requests refused unread in `refused_requests`. Queues each alert not
/// sent before.
pub fn write(c: &Connection, flush: &Flush) -> Result<()> {
    for r in &flush.refused {
        let day_ms = time::midnight(r.day) / 1_000_000;
        client_report::add_own(c, r.project, day_ms, r.reason, r.category, r.quantity)?;
    }
    let mut q = c.prepare_cached(
        "INSERT INTO refused_requests(day, project_id, reason, quantity) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT DO UPDATE SET quantity=quantity+excluded.quantity",
    )?;
    for r in &flush.requests {
        let day_ms = time::midnight(r.day) / 1_000_000;
        q.execute((day_ms, r.project, r.reason, r.quantity))?;
    }
    for a in &flush.alerts {
        notify(c, a)?;
    }
    Ok(())
}

/// Marks `a` sent in `quota_alerts`, `Reached` with its warning, and
/// queues its notice when the mark is new. A deleted project gets none.
fn notify(c: &Connection, a: &Alert) -> Result<()> {
    let mut mark = c.prepare_cached(
        "INSERT OR IGNORE INTO quota_alerts(day, project_id, signal, kind, quota)
         SELECT ?1, id, ?3, ?4, ?5 FROM projects WHERE id=?2",
    )?;
    let (day, signal) = (a.day.to_string(), name(a.signal));
    let at = i64::try_from(a.quota).unwrap_or(i64::MAX);
    let new = mark.execute((&day, a.project, signal, a.crossed.name(), at))? == 1;
    if a.crossed == Crossed::Reached {
        mark.execute((&day, a.project, signal, Crossed::Warn.name(), at))?;
    }
    if !new {
        return Ok(());
    }
    let quota = alert::Quota {
        signal,
        used: a.used,
        quota: a.quota,
        percent: status::percent(a.used, a.quota),
        warn_percent: a.warn,
        resets_at: time::rfc3339(a.resets_at),
    };
    let notice = match a.crossed {
        Crossed::Warn => alert::Notice::QuotaWarn(quota),
        Crossed::Reached => alert::Notice::QuotaReached(quota),
    };
    alert::store::notify(c, a.project, &notice, a.at / 1_000_000)
}

/// Deletes `quota_alerts` rows older than [`ALERT_DAYS`], and the counts
/// of requests refused unread older than `client_reports` keeps its
/// days, before `at_ms`.
pub fn prune(c: &Connection, at_ms: i64) -> Result<()> {
    let today = time::day_of(at_ms.saturating_mul(1_000_000));
    if let Ok(cutoff) = today.checked_sub(jiff::ToSpan::days(ALERT_DAYS)) {
        c.execute(
            "DELETE FROM quota_alerts WHERE day<?1",
            [cutoff.to_string()],
        )?;
    }
    let day_ms = time::DAY_NS / 1_000_000;
    let cutoff = at_ms
        .div_euclid(day_ms)
        .saturating_sub(client_report::KEEP_DAYS)
        * day_ms;
    c.execute("DELETE FROM refused_requests WHERE day<?1", [cutoff])?;
    Ok(())
}

#[cfg(test)]
#[path = "quota/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "quota/alert_tests.rs"]
mod alert_tests;

#[cfg(test)]
#[path = "quota/spike_tests.rs"]
mod spike_tests;
