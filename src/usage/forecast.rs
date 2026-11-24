//! When the disk fills: a pure function of what Davy measured, so every
//! case has a test with fixed numbers.
//!
//! Logs of all projects share one index per day, so Davy cannot measure
//! one project's bytes on disk. The forecast takes the growth of all logs
//! together and caps it with the longest log retention of any project, a
//! bound no split of the growth can exceed.

use jiff::{ToSpan as _, civil::Date};
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
    /// cannot.
    pub fn check<'a>(&self, mut slugs: impl Iterator<Item = &'a str>) -> Result<(), String> {
        if let Some(slug) = &self.project {
            if self.logs_bytes_per_day.is_some() {
                return Err(ONE_PROJECTS_BYTES.to_owned());
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

#[derive(Clone, Debug, PartialEq)]
pub struct Forecast {
    /// Bytes of logs on disk per day, all projects together.
    pub growth: u64,
    /// The longest log retention of any project, which caps the growth.
    pub longest_days: u32,
    /// The most Davy takes once retention deletes as much as arrives.
    pub steady_state: u64,
    pub headroom: u64,
    /// `None` when the disk never fills at this rate.
    pub days_left: Option<u64>,
    /// The UTC day of `days_left`, `None` when it never fills or the day
    /// is past the last date there is.
    pub full_on: Option<Date>,
}

/// The forecast, or why there is none.
pub fn forecast(input: &Input, what_if: &WhatIf) -> Result<Forecast, String> {
    what_if.check(input.projects.iter().map(|(s, _)| s.as_str()))?;
    // What is on disk follows the settings, the cap follows the what-if.
    let shortest = input.projects.iter().map(|(_, n)| *n).min();
    let longest = input
        .projects
        .iter()
        .map(|(slug, n)| match what_if.logs_days {
            Some(days) if what_if.project.as_ref().is_none_or(|p| p == slug) => days,
            _ => *n,
        })
        .max()
        .unwrap_or(what_if.logs_days.unwrap_or(input.default_days));
    let growth = match what_if.logs_bytes_per_day {
        Some(bytes) => bytes,
        None => measured(input, shortest.unwrap_or(input.default_days))?,
    };
    let steady_state = growth
        .saturating_mul(u64::from(longest))
        .saturating_add(input.fixed);
    let headroom = input.total / 100 * HEADROOM_PERCENT;
    let days_left = days_left(input, growth, steady_state, headroom);
    let full_on = days_left.and_then(|n| {
        let n = i64::try_from(n).ok()?;
        input.today.checked_add(n.days()).ok()
    });
    Ok(Forecast {
        growth,
        longest_days: longest,
        steady_state,
        headroom,
        days_left,
        full_on,
    })
}

/// The median size of the newest closed days, at most [`DAYS`], that no
/// retention has cut yet: those within the `shortest` retention of any
/// project. An older day lost the logs of the projects that keep fewer
/// days, and would make the growth look smaller than it is.
fn measured(input: &Input, shortest: u32) -> Result<u64, String> {
    let first = input
        .today
        .checked_sub(i64::from(shortest).days())
        .unwrap_or(Date::MIN);
    let sizes: Vec<u64> = input
        .closed
        .iter()
        .rev()
        .filter(|(day, _)| *day >= first && *day < input.today)
        .take(DAYS)
        .map(|(_, bytes)| *bytes)
        .collect();
    // With 1 day of retention, yesterday is the only day not cut.
    let need = if shortest >= 2 { 2 } else { 1 };
    match median(&sizes) {
        Some(m) if sizes.len() >= need => Ok(m),
        _ => Err(format!(
            "closed log days since {first}, the first day no retention has cut: the disk has {}, a forecast needs {need}",
            sizes.len()
        )),
    }
}

/// Days until used space reaches total minus headroom, while Davy grows by
/// `growth` a day until it reaches `steady`. `None` when it never does.
fn days_left(input: &Input, growth: u64, steady: u64, headroom: u64) -> Option<u64> {
    let limit = input.total.saturating_sub(headroom);
    if input.used >= limit {
        return Some(0);
    }
    let room = steady.saturating_sub(input.davy);
    if growth == 0 || input.used.saturating_add(room) < limit {
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
