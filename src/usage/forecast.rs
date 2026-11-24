//! When the disk fills: a pure function of what Davy measured, so every
//! case has a test with fixed numbers.

use jiff::{ToSpan as _, civil::Date};
use rmcp::schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Closed log days the growth is the median of.
pub const DAYS: usize = 7;
/// The share of the filesystem the forecast keeps free.
pub const HEADROOM_PERCENT: u64 = 10;
pub const HEADROOM_REASON: &str = "10% of the filesystem stays free: filesystems reserve blocks, and index merges and SQLite checkpoints need room for a copy of what they rewrite";

/// One project's share of the log growth and how long it keeps logs.
#[derive(Clone, Debug, PartialEq)]
pub struct Project {
    pub slug: String,
    /// Log bytes counted over the closed days the growth comes from.
    pub log_bytes: u64,
    pub logs_days: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Input {
    pub today: Date,
    /// Sizes on disk of the newest closed log days, at most [`DAYS`].
    pub closed: Vec<u64>,
    /// What Davy's files take now.
    pub davy: u64,
    /// What SQLite, uploads and other files take now. They are taken as
    /// they are.
    pub fixed: u64,
    /// Used and total bytes of the filesystem.
    pub used: u64,
    pub total: u64,
    /// Every project, with its log retention.
    pub projects: Vec<Project>,
    /// The server's log retention, for growth no project was counted for.
    pub default_days: u32,
}

/// One input changed, to see what it would do.
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
pub struct WhatIf {
    /// The project the change applies to. Without it, every project.
    pub project: Option<String>,
    /// Days of logs the project, or every project, would keep.
    pub logs_days: Option<u32>,
    /// Bytes of logs on disk the project, or all projects together, would
    /// add per day.
    pub logs_bytes_per_day: Option<u64>,
}

impl WhatIf {
    pub fn is_empty(&self) -> bool {
        self.logs_days.is_none() && self.logs_bytes_per_day.is_none()
    }
}

/// The log growth of one project, or with `project` `None` of logs no
/// project was counted for.
#[derive(Clone, Debug, PartialEq)]
pub struct Growth {
    pub project: Option<String>,
    pub bytes_per_day: u64,
    pub logs_days: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Forecast {
    /// Bytes of logs on disk per day, all projects together.
    pub growth: u64,
    pub per_project: Vec<Growth>,
    /// What Davy takes once retention deletes as much as arrives.
    pub steady_state: u64,
    pub headroom: u64,
    /// `None` when the disk never fills at this rate.
    pub days_left: Option<u64>,
    pub full_on: Option<Date>,
}

/// The forecast, or why there is none.
pub fn forecast(input: &Input, what_if: &WhatIf) -> Result<Forecast, String> {
    if let Some(slug) = &what_if.project
        && !input.projects.iter().any(|p| &p.slug == slug)
    {
        return Err(crate::retention::unknown(slug));
    }
    if what_if
        .logs_days
        .is_some_and(|n| !(1..=crate::retention::MAX_DAYS).contains(&n))
    {
        return Err(format!(
            "logs_days must be 1 to {}",
            crate::retention::MAX_DAYS
        ));
    }
    let total_override = what_if
        .logs_bytes_per_day
        .filter(|_| what_if.project.is_none());
    let growth = match (median(&input.closed), total_override) {
        (_, Some(bytes)) => bytes,
        (Some(m), None) if input.closed.len() >= 2 => m,
        _ => {
            return Err(format!(
                "a forecast needs at least 2 closed log days, and the disk has {}",
                input.closed.len()
            ));
        }
    };
    let per_project = apply(split(input, growth), input, what_if);
    let growth = per_project.iter().map(|g| g.bytes_per_day).sum::<u64>();
    let steady_state = per_project
        .iter()
        .map(|g| g.bytes_per_day.saturating_mul(u64::from(g.logs_days)))
        .fold(input.fixed, u64::saturating_add);
    let headroom = input.total / 100 * HEADROOM_PERCENT;
    let days_left = days_left(input, growth, steady_state, headroom);
    let full_on = days_left.and_then(|n| {
        let n = i64::try_from(n).ok()?;
        input.today.checked_add(n.days()).ok()
    });
    Ok(Forecast {
        growth,
        per_project,
        steady_state,
        headroom,
        days_left,
        full_on,
    })
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

/// `growth` split by each project's share of counted log bytes. Without
/// counts, all of it follows the server's default retention.
fn split(input: &Input, growth: u64) -> Vec<Growth> {
    let counted = input
        .projects
        .iter()
        .map(|p| u128::from(p.log_bytes))
        .sum::<u128>();
    if counted == 0 {
        return vec![Growth {
            project: None,
            bytes_per_day: growth,
            logs_days: input.default_days,
        }];
    }
    input
        .projects
        .iter()
        .filter(|p| p.log_bytes > 0)
        .map(|p| Growth {
            project: Some(p.slug.clone()),
            bytes_per_day: u64::try_from(u128::from(growth) * u128::from(p.log_bytes) / counted)
                .unwrap_or(u64::MAX),
            logs_days: p.logs_days,
        })
        .collect()
}

/// The growth with the what-if's project changes applied.
fn apply(mut growth: Vec<Growth>, input: &Input, what_if: &WhatIf) -> Vec<Growth> {
    let Some(slug) = &what_if.project else {
        if let Some(days) = what_if.logs_days {
            for g in &mut growth {
                g.logs_days = days;
            }
        }
        return growth;
    };
    let found = growth.iter().position(|g| g.project.as_ref() == Some(slug));
    let index = if let Some(i) = found {
        i
    } else {
        let days = input
            .projects
            .iter()
            .find(|p| &p.slug == slug)
            .map_or(input.default_days, |p| p.logs_days);
        growth.push(Growth {
            project: Some(slug.clone()),
            bytes_per_day: 0,
            logs_days: days,
        });
        growth.len() - 1
    };
    if let Some(days) = what_if.logs_days {
        growth[index].logs_days = days;
    }
    if let Some(bytes) = what_if.logs_bytes_per_day {
        growth[index].bytes_per_day = bytes;
    }
    growth
}

#[cfg(test)]
#[path = "forecast_tests.rs"]
mod tests;
