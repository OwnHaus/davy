//! Client reports: how many events an SDK dropped before sending, and why.
//!
//! The SDKs send them by default. Each says, per reason and category, how
//! many items never left the app, such as errors `before_send` dropped or
//! logs a full queue lost. Davy adds them up per project and UTC day, so an
//! agent can tell "no errors" from "the SDK threw them away".

use crate::event::Timestamp;
use anyhow::Result;
use rusqlite::{Connection, params};
use serde::Deserialize;

const DAY_MS: i64 = 86_400_000;
/// Entries read from one report. An SDK sends one per reason and category,
/// and has a few dozen of each.
const MAX_ENTRIES: usize = 100;
const MAX_NAME: usize = 64;
/// Reason and category pairs kept per project and day. The DSN of a browser
/// app is public, so anyone can send reports with made-up names.
const MAX_PAIRS: i64 = 100;
/// The most one entry adds. An SDK reports every minute or so, and a sum
/// of these cannot overflow a row in any number of years.
const MAX_QUANTITY: u64 = 1_000_000;
/// Days a row is kept.
pub const KEEP_DAYS: i64 = 90;
/// The reasons of the counts Davy writes itself, for its own limits and
/// refusals. An SDK's entry with one is skipped, so no client can add to
/// them.
pub const OWN_PREFIX: &str = "davy_";

/// The count of one reason and category in one report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Discarded {
    pub reason: String,
    pub category: String,
    pub quantity: i64,
}

#[derive(Deserialize)]
struct Raw {
    #[serde(default)]
    discarded_events: Vec<RawDiscarded>,
}

#[derive(Deserialize)]
struct RawDiscarded {
    reason: String,
    category: String,
    quantity: u64,
}

/// The entries of a client report Davy keeps. `None` when the payload is not
/// a report. Entries with a name that is not short snake case, a reason
/// that starts with [`OWN_PREFIX`] or a quantity of 0 are skipped. A
/// quantity over [`MAX_QUANTITY`] counts as that.
pub fn parse(payload: &[u8]) -> Option<Vec<Discarded>> {
    let raw: Raw = serde_json::from_slice(payload).ok()?;
    Some(
        raw.discarded_events
            .into_iter()
            .filter(|d| {
                name(&d.reason)
                    && !d.reason.starts_with(OWN_PREFIX)
                    && name(&d.category)
                    && d.quantity > 0
            })
            .take(MAX_ENTRIES)
            .map(|d| Discarded {
                reason: d.reason,
                category: d.category,
                quantity: i64::try_from(d.quantity.min(MAX_QUANTITY)).unwrap_or(0),
            })
            .collect(),
    )
}

fn name(s: &str) -> bool {
    (1..=MAX_NAME).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// Adds the entries to the counts of `project` on the UTC day of `at`. A new
/// pair past [`MAX_PAIRS`] for that day is not counted.
pub fn add(c: &Connection, project: i64, at: Timestamp, entries: &[Discarded]) -> Result<()> {
    let day = at.div_euclid(DAY_MS) * DAY_MS;
    let mut add = c.prepare_cached(
        "INSERT INTO client_reports(day, project_id, reason, category, quantity)
         SELECT ?1, ?2, ?3, ?4, ?5
         WHERE EXISTS (SELECT 1 FROM client_reports WHERE day=?1 AND project_id=?2 AND reason=?3 AND category=?4)
            OR (SELECT count(*) FROM client_reports WHERE day=?1 AND project_id=?2) < ?6
         ON CONFLICT DO UPDATE SET quantity=quantity+excluded.quantity",
    )?;
    for d in entries {
        add.execute(params![
            day, project, d.reason, d.category, d.quantity, MAX_PAIRS
        ])?;
    }
    Ok(())
}

/// Deletes the days older than [`KEEP_DAYS`] before `at`.
pub fn prune(c: &Connection, at: Timestamp) -> Result<()> {
    let cutoff = at.div_euclid(DAY_MS).saturating_sub(KEEP_DAYS) * DAY_MS;
    c.execute("DELETE FROM client_reports WHERE day<?1", [cutoff])?;
    Ok(())
}

#[cfg(test)]
#[path = "client_report_tests.rs"]
mod tests;
