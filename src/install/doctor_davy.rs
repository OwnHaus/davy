//! The doctor check `davy`: what Davy reported of itself in the last 24
//! hours, from its project `davy`, and crash files that wait for `davy
//! serve`. A crash warns and names how to read it.

use super::{Check, Status, not_ok, ok};
use crate::self_report::{
    crash,
    status::{self, Health},
};
use std::path::Path;

const NAME: &str = "davy";

/// What Davy reported of itself in the 24 hours before `now_ms`, read from
/// the data directory `data`, which `d` names for a shell.
pub(super) fn davy(data: &Path, d: &str, now_ms: i64) -> Check {
    let read = if data.join(crate::store::DB_FILE).exists() {
        crate::query::open(data).and_then(|c| status::health(&c, data, now_ms))
    } else {
        crash::pending(&crash::dir(data))
            .map(|files| Health::waiting(files.len()))
            .map_err(anyhow::Error::from)
    };
    match read {
        Ok(health) => check(&health, d),
        Err(e) => not_ok(
            NAME,
            Status::Warn,
            format!("what Davy reported of itself was not read: {e:#}"),
            format!("make {d} readable by the user doctor runs as"),
        ),
    }
}

/// The check for `h`: a warning for a crash in the window or a crash file
/// that waits, else the counts of Davy's own lines.
fn check(h: &Health, d: &str) -> Check {
    let lines = format!(
        "{} errors and {} warnings of Davy's own in the last 24 hours",
        h.errors_24h, h.warnings_24h
    );
    let waiting = match h.crashes_waiting {
        0 => String::new(),
        n => format!("; {n} crash files wait in {d}/crashes"),
    };
    if let Some(last) = h.crashes_24h.first() {
        let more = match h.crashes_24h.len() - 1 {
            0 => String::new(),
            n => format!(" and {n} other crash issues"),
        };
        return not_ok(
            NAME,
            Status::Warn,
            format!(
                "Davy crashed in the last 24 hours: issue {} {:?}, {} events, the last at {}, recorded at {}{more}; {lines}{waiting}",
                last.issue_id, last.title, last.event_count, last.last_seen, last.recorded_at
            ),
            format!(
                "davy context {} --data {d}, or the MCP call issue_context with issue_id {}",
                last.issue_id, last.issue_id
            ),
        );
    }
    if h.crashes_waiting > 0 {
        return not_ok(
            NAME,
            Status::Warn,
            format!(
                "Davy crashed: {} crash files wait in {d}/crashes to be recorded; {lines}",
                h.crashes_waiting
            ),
            format!(
                "davy serve --data {d} records them in the project davy within 5 seconds, unless it runs with --no-self-report"
            ),
        );
    }
    if h.quiet() {
        return ok(
            NAME,
            "no crash, error or warning of Davy's own in the last 24 hours",
        );
    }
    ok(
        NAME,
        format!(
            "no crash in the last 24 hours; {lines}, listed by davy logs --data {d} --project davy --level warn"
        ),
    )
}

#[cfg(test)]
#[path = "doctor_davy_tests.rs"]
mod tests;
