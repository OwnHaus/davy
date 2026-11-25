//! The log retention a project's searches need (`docs/design.md`,
//! "Retention fit"): the oldest data any search of the last
//! [`LOOK_BACK_DAYS`] days read, against the days the project keeps.
//! Read from `search_days`, which the count trim of `searches` leaves
//! alone.

use crate::time::{self, DAY_NS};
use anyhow::Result;
use jiff::civil::Date;
use rusqlite::{Connection, params};

/// The days of searches read.
pub const LOOK_BACK_DAYS: i64 = 30;
/// Searches recorded for fewer whole days than this suggest nothing: a
/// week without a search says little.
pub const MIN_HISTORY_DAYS: i64 = 7;
/// The retentions suggested, the smallest that fits first.
pub const STEPS: [u32; 7] = [7, 14, 30, 60, 90, 180, 365];

#[derive(Debug, PartialEq)]
pub struct Fit {
    pub project: String,
    pub logs_days: u32,
    /// How far back the oldest search read, in whole days rounded up.
    pub oldest_read_days: u32,
    /// Searches of the project, or of every project, in the look-back.
    pub searches: u64,
    /// The whole days between the first day `search_days` still holds and
    /// today, at most [`LOOK_BACK_DAYS`].
    pub history_days: u32,
    pub suggest: u32,
    /// Disk bytes the days the cut frees take, at `ratio` ppm of the raw
    /// bytes counted on them.
    pub saves: u64,
}

/// The day `days` days before `now_ns`, as log retention counts it.
fn back(now_ns: i64, days: i64) -> Date {
    time::day_of(now_ns.saturating_sub(DAY_NS.saturating_mul(days)))
}

/// What project `id` would save keeping logs for the smallest of [`STEPS`]
/// that is twice the oldest read and at least 7 days, if that is under its
/// `logs_days` and frees bytes. `None` before [`MIN_HISTORY_DAYS`] of
/// recorded searches, and with no search of the project or of every
/// project in the look-back: a read Davy does not record is no reason to
/// cut.
pub fn fit(
    c: &Connection,
    (id, project): (i64, &str),
    logs_days: u32,
    ratio: u64,
    now_ns: i64,
) -> Result<Option<Fit>> {
    let history = history_days(c, now_ns)?;
    if history < MIN_HISTORY_DAYS {
        return Ok(None);
    }
    let (searches, oldest): (i64, Option<i64>) = c.query_row(
        "SELECT COALESCE(SUM(searches), 0), MAX(oldest_read_days) FROM search_days
         WHERE day>=?1 AND project_id IN (0, ?2)",
        params![back(now_ns, LOOK_BACK_DAYS).to_string(), id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let (Ok(searches @ 1..), Some(oldest)) = (u64::try_from(searches), oldest) else {
        return Ok(None);
    };
    let need = oldest.saturating_mul(2).max(7);
    let Some(&suggest) = STEPS
        .iter()
        .find(|s| i64::from(**s) >= need && **s < logs_days)
    else {
        return Ok(None);
    };
    // A day is kept while it is not before `back(days)`, so the cut frees
    // `back(logs_days)` up to the day before `back(suggest)`.
    let raw: i64 = c.query_row(
        "SELECT COALESCE(SUM(bytes), 0) FROM log_days
         WHERE project_id=?1 AND cut=0 AND day>=?2 AND day<?3",
        params![
            id,
            back(now_ns, logs_days.into()).to_string(),
            back(now_ns, suggest.into()).to_string()
        ],
        |r| r.get(0),
    )?;
    let raw = u128::try_from(raw).unwrap_or(0);
    let saves = u64::try_from(raw * u128::from(ratio) / 1_000_000).unwrap_or(u64::MAX);
    if saves == 0 {
        return Ok(None);
    }
    Ok(Some(Fit {
        project: project.to_owned(),
        logs_days,
        oldest_read_days: u32::try_from(oldest).unwrap_or(u32::MAX),
        searches,
        history_days: u32::try_from(history).unwrap_or(0),
        suggest,
        saves,
    }))
}

/// The whole days `search_days` covers before today, at most
/// [`LOOK_BACK_DAYS`]: its first day is counted out, since the first
/// search may have come late in it.
fn history_days(c: &Connection, now_ns: i64) -> Result<i64> {
    let first: Option<String> =
        c.query_row("SELECT MIN(day) FROM search_days", [], |r| r.get(0))?;
    let Some(first) = first.and_then(|d| d.parse::<Date>().ok()) else {
        return Ok(0);
    };
    let today = time::midnight(time::day_of(now_ns));
    let days = today.saturating_sub(time::midnight(first)) / DAY_NS;
    Ok((days - 1).clamp(0, LOOK_BACK_DAYS))
}
