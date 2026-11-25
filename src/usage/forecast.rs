//! When the disk fills: a pure function of what Davy measured, so every
//! case has a test with fixed numbers.
//!
//! Logs of all projects share one index per day, so Davy cannot measure
//! one project's bytes on disk. The forecast takes the growth of all logs
//! together and caps it with the longest log retention of any project, a
//! bound no split of the growth can exceed. When projects keep logs for
//! different times, the forecast is therefore a worst case: at the current
//! rate of logs, the disk fills on its day or later, or never.
//!
//! The growth itself comes from [`growth`]: raw bytes per day times how
//! they pack, or the disk sizes of the closed days.

mod growth;

pub use growth::{Basis, Confidence, LOOK_BACK, Logs, RawDay, Spread, hours};

use crate::usage::footprint::Prior;
use jiff::{Span, ToSpan as _, civil::Date};
use rmcp::schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Closed log days the growth is the median of.
pub const DAYS: usize = 7;
/// The share of the filesystem the forecast keeps free.
pub const HEADROOM_PERCENT: u64 = 10;
pub const HEADROOM_REASON: &str = "10% of the filesystem stays free: filesystems reserve blocks, and index merges and SQLite checkpoints need room for a copy of what they rewrite";

#[derive(Clone, Debug, PartialEq)]
pub struct Input {
    pub today: Date,
    /// Each closed log day on disk and its size, oldest first.
    pub closed: Vec<(Date, u64)>,
    /// What Davy's files take now.
    pub davy: u64,
    /// What SQLite, uploads and other files take now. They are taken as
    /// they are.
    pub fixed: u64,
    /// Used and total bytes of the filesystem.
    pub used: u64,
    pub total: u64,
    /// Every project's slug and the days of logs it keeps.
    pub projects: Vec<(String, u32)>,
    /// The server's log retention, which a server without projects keeps.
    pub default_days: u32,
    /// Raw bytes per index day and project from `log_days`, the newest
    /// [`LOOK_BACK`] days and on, oldest first.
    pub raw: Vec<RawDay>,
    /// When Davy started counting raw bytes, ns. `None` on a server whose
    /// writer never counted.
    pub counted_since: Option<i64>,
    /// Now, ns, for the hours counted on a new server.
    pub now: i64,
    /// How logs pack where the server has not measured it.
    pub prior: Prior,
}

/// One input changed, to see what it would do.
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
pub struct WhatIf {
    /// The project `logs_days` applies to. Without it, every project.
    pub project: Option<String>,
    /// Days of logs the project, or every project, would keep.
    pub logs_days: Option<u32>,
    /// Bytes of logs on disk all projects together would add per day.
    pub logs_bytes_per_day: Option<u64>,
}

impl WhatIf {
    pub fn is_empty(&self) -> bool {
        self.logs_days.is_none() && self.logs_bytes_per_day.is_none()
    }

    /// Why the what-if cannot run against the projects `slugs`, if it
    /// cannot. [`forecast`] takes a what-if that passed.
    pub fn check<'a>(&self, mut slugs: impl Iterator<Item = &'a str>) -> Result<(), String> {
        if let Some(slug) = &self.project {
            if self.logs_bytes_per_day.is_some() {
                return Err(ONE_PROJECTS_BYTES.to_owned());
            }
            if self.logs_days.is_none() {
                return Err(format!(
                    "a what-if for project {slug} needs logs_days, the days of logs it would keep"
                ));
            }
            if !slugs.any(|s| s == slug) {
                return Err(crate::retention::unknown(slug));
            }
        }
        if self
            .logs_days
            .is_some_and(|n| !(1..=crate::retention::MAX_DAYS).contains(&n))
        {
            return Err(format!(
                "logs_days must be 1 to {}",
                crate::retention::MAX_DAYS
            ));
        }
        Ok(())
    }
}

const ONE_PROJECTS_BYTES: &str = "logs_bytes_per_day is for all projects together: logs of every project share one index per day, so Davy cannot tell one project's bytes on disk";

/// Why there is no forecast.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NoForecast {
    /// Too few closed log days, as on a new server: nothing is wrong.
    History(String),
    /// The disk walk stopped at its entry or time limit. The report finds
    /// this and the two below, [`forecast`] never returns one.
    Stopped(String),
    /// Part of `logs` is left out of the walk, such as a link, a mount or
    /// a directory Davy may not read.
    LogsLeftOut(String),
    /// The disk or the database could not be read at all.
    Failed(String),
}

impl std::fmt::Display for NoForecast {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::History(why)
            | Self::Stopped(why)
            | Self::LogsLeftOut(why)
            | Self::Failed(why) => f.write_str(why),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Forecast {
    /// Bytes of logs on disk per day, all projects together: the growth
    /// the bound decides on, see [`Logs::bound`].
    pub growth: u64,
    /// The longest log retention of any project, which caps the growth.
    pub longest_days: u32,
    /// The shortest log retention of any project. Below `longest_days`,
    /// the forecast is a worst case: see [`Forecast::worst_case`].
    pub shortest_days: u32,
    /// The most Davy takes once retention deletes as much as arrives: the
    /// longest retention's closed days and today, each a day's growth.
    pub steady_state: u64,
    pub headroom: u64,
    /// `None` when logs never fill the disk at this rate.
    pub days_left: Option<u64>,
    /// The UTC day of `days_left`. `days_left` is at most the longest
    /// retention and a day, so the day is always a date.
    pub full_on: Option<Date>,
    /// Why the forecast cannot show what a what-if for one project does,
    /// when it cannot.
    pub unseen: Option<String>,
    /// Where the growth comes from. `None` for a what-if of the bytes per
    /// day, which states the growth.
    pub logs: Option<Logs>,
    /// `full_on` at the high growth and at the low growth of the window,
    /// `None` for never: the earliest and the latest day.
    pub full_on_range: [Option<Date>; 2],
}

impl Forecast {
    /// Whether projects keep logs for different times. The forecast then
    /// takes all logs as kept the longest of them, so `days_left` is the
    /// fewest days there can be at the current rate of logs: the disk
    /// fills that day or later, or never.
    pub fn worst_case(&self) -> bool {
        self.shortest_days < self.longest_days
    }
}

/// The forecast for a what-if that passed [`WhatIf::check`], or why there
/// is none.
pub fn forecast(input: &Input, what_if: &WhatIf) -> Result<Forecast, NoForecast> {
    // What is on disk follows the settings, the cap follows the what-if.
    let shortest = input.projects.iter().map(|(_, n)| *n).min();
    let after = kept_after(input, what_if);
    let longest = after.iter().map(|(_, n)| *n).max();
    let longest = longest.unwrap_or(what_if.logs_days.unwrap_or(input.default_days));
    let logs = match what_if.logs_bytes_per_day {
        Some(_) => None,
        None => Some(growth::logs(
            input,
            measured(input, shortest.unwrap_or(input.default_days)),
        )?),
    };
    let (growth, range) = match (&logs, what_if.logs_bytes_per_day) {
        (Some(l), _) => (l.bound(), l.growth),
        (None, bytes) => (bytes.unwrap_or(0), Spread::flat(bytes.unwrap_or(0))),
    };
    let headroom = input.total / 100 * HEADROOM_PERCENT;
    let run = |growth| run(input, growth, longest, headroom);
    let (steady_state, days_left, full_on) = run(growth);
    Ok(Forecast {
        growth,
        longest_days: longest,
        shortest_days: after.iter().map(|(_, n)| *n).min().unwrap_or(longest),
        steady_state,
        headroom,
        days_left,
        full_on,
        unseen: unseen(input, what_if, &after, longest),
        logs,
        full_on_range: [run(range.high).2, run(range.low).2],
    })
}

/// The steady state, the days left and the day the disk fills at `growth`
/// with logs kept `longest` days.
fn run(
    input: &Input,
    growth: u64,
    longest: u32,
    headroom: u64,
) -> (u64, Option<u64>, Option<Date>) {
    // Retention of N days deletes a day once it is more than N days old,
    // so the disk holds N closed days and today.
    let steady_state = growth
        .saturating_mul(u64::from(longest) + 1)
        .saturating_add(input.fixed);
    let days_left = days_left(input, growth, steady_state, headroom);
    let full_on = days_left.map(|n| {
        let span = i64::try_from(n)
            .ok()
            .and_then(|n| Span::new().try_days(n).ok());
        span.and_then(|s| input.today.checked_add(s).ok())
            .unwrap_or(Date::MAX)
    });
    (steady_state, days_left, full_on)
}

/// Each project's slug and the days of logs it keeps with the what-if.
fn kept_after<'a>(input: &'a Input, what_if: &WhatIf) -> Vec<(&'a str, u32)> {
    input
        .projects
        .iter()
        .map(|(slug, n)| match what_if.logs_days {
            Some(days) if what_if.project.as_ref().is_none_or(|p| p == slug) => {
                (slug.as_str(), days)
            }
            _ => (slug.as_str(), *n),
        })
        .collect()
}

/// Why the what-if for one project leaves the forecast as it is, when it
/// does: another project keeps logs `longest` days with it and without
/// it. The bound already takes the project's logs as kept that long, and
/// Davy cannot tell the project's share of the growth.
fn unseen(input: &Input, what_if: &WhatIf, after: &[(&str, u32)], longest: u32) -> Option<String> {
    let project = what_if.project.as_deref()?;
    let now = input.projects.iter().find(|(s, _)| s == project)?.1;
    if what_if.logs_days == Some(now) {
        return None;
    }
    let before = input.projects.iter().map(|(_, n)| *n).max()?;
    let (holder, _) = after
        .iter()
        .find(|(slug, n)| *slug != project && *n == longest)?;
    (before == longest).then(|| {
        format!(
            "the forecast cannot show this what-if: it takes the logs of every project as kept {longest} days, as long as {holder} keeps them, and logs of all projects share one index per day, so Davy cannot tell how much of the growth is {project}'s. The worst case stays as it is, though {project}'s own logs would take less disk with a shorter retention and more with a longer one"
        )
    })
}

/// What the disk sizes of the closed days measured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Direct {
    /// Their median size.
    pub growth: u64,
    /// How many closed days the median is of.
    pub days: usize,
    /// The oldest of them.
    pub oldest: Date,
}

/// The median size of the newest closed days, at most [`DAYS`], that no
/// retention has cut yet: those within the `shortest` retention of any
/// project. An older day lost the logs of the projects that keep fewer
/// days, and would make the growth look smaller than it is.
fn measured(input: &Input, shortest: u32) -> Result<Direct, NoForecast> {
    let first = input
        .today
        .checked_sub(i64::from(shortest).days())
        .unwrap_or(Date::MIN);
    let days: Vec<&(Date, u64)> = input
        .closed
        .iter()
        .rev()
        .filter(|(day, _)| *day >= first && *day < input.today)
        .take(DAYS)
        .collect();
    let sizes: Vec<u64> = days.iter().map(|(_, bytes)| *bytes).collect();
    // With 1 day of retention, yesterday is the only day not cut.
    let need = if shortest >= 2 { 2 } else { 1 };
    match (median(&sizes), days.last()) {
        (Some(m), Some((oldest, _))) if sizes.len() >= need => Ok(Direct {
            growth: m,
            days: sizes.len(),
            oldest: *oldest,
        }),
        _ => Err(NoForecast::History(format!(
            "closed log days since {first}, the first day no retention has cut: the disk has {}, a forecast needs {need}",
            sizes.len()
        ))),
    }
}

/// Days until used space reaches total minus headroom, while Davy grows by
/// `growth` a day until it reaches `steady`. `None` when it never does,
/// which includes a disk already past the limit while Davy no longer
/// grows: other files filled it, and logs add nothing more.
fn days_left(input: &Input, growth: u64, steady: u64, headroom: u64) -> Option<u64> {
    let limit = input.total.saturating_sub(headroom);
    let room = steady.saturating_sub(input.davy);
    if growth == 0 || room == 0 {
        return None;
    }
    if input.used >= limit {
        return Some(0);
    }
    if input.used.saturating_add(room) < limit {
        return None;
    }
    Some((limit - input.used).div_ceil(growth))
}

/// The middle value, or the mean of the two middle ones.
fn median(sizes: &[u64]) -> Option<u64> {
    let mut sorted = sizes.to_vec();
    sorted.sort_unstable();
    let mid = sorted.len() / 2;
    match sorted.len() {
        0 => None,
        n if n % 2 == 1 => Some(sorted[mid]),
        _ => {
            let sum = u128::from(sorted[mid - 1]) + u128::from(sorted[mid]);
            u64::try_from(sum / 2).ok()
        }
    }
}

#[cfg(test)]
#[path = "forecast_tests.rs"]
mod tests;
