//! Daily quotas and spike protection per project (`docs/design.md`,
//! "Quotas and spike protection"). A person sets them with `davy project
//! quota`. Ingest checks them in memory against today's counts, which the
//! usage counter keeps as it counts what ingest accepts, so the hot path
//! never reads SQLite. What ingest refuses is counted in `client_reports`
//! under [`QUOTA`], in items, and a log request refused before its body
//! was read in `refused_requests`, in requests, as is a `429` of a full
//! body budget or queue under [`BUSY`].

use crate::{
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
pub mod status;
pub use gate::{Flush, Gate, Limits, Refusal, Requests};
pub use settings::Settings;

/// The client report reason of items refused past a daily quota.
pub const QUOTA: &str = "davy_quota";
/// The reason of requests refused with `429` because the body budget or a
/// queue was full, counted in `refused_requests` under the project their
/// key picked, or [`NO_PROJECT`] when the key was not read yet.
pub const BUSY: &str = "davy_busy";
/// The project of a busy request whose key was not read yet.
pub const NO_PROJECT: i64 = 0;
/// How soon a change from `davy project quota` reaches ingest.
pub const RELOAD_EVERY: Duration = Duration::from_secs(5);

/// Settings per project, the table that marks each alert sent, so a
/// quota warns once per project, signal, UTC day and quota across
/// restarts, and the count of requests refused unread or busy, per UTC
/// day (unix ms at midnight) like `client_reports`. Its `project_id` has
/// no reference, as a busy request may have no project.
pub const MIGRATION: &str = "ALTER TABLE projects ADD COLUMN quota_errors INTEGER CHECK(quota_errors > 0);
ALTER TABLE projects ADD COLUMN quota_logs INTEGER CHECK(quota_logs > 0);
ALTER TABLE projects ADD COLUMN quota_warn INTEGER NOT NULL DEFAULT 80 CHECK(quota_warn BETWEEN 1 AND 100);
ALTER TABLE projects ADD COLUMN spike INTEGER CHECK(spike >= 0);
ALTER TABLE projects ADD COLUMN spike_logs INTEGER CHECK(spike_logs > 0);
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
    gate.load(settings::all(c)?);
    let today = time::day_of(now_ns);
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
            Ok(all) => gate.load(all),
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

/// Counts the refusals in `client_reports`, Davy's own reasons are few,
/// so they skip the cap on pairs a day an SDK's reports have, and the
/// requests refused unread in `refused_requests`.
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
    Ok(())
}

/// Deletes the counts of requests refused unread older than
/// `client_reports` keeps its days, before `at_ms`.
pub fn prune(c: &Connection, at_ms: i64) -> Result<()> {
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
