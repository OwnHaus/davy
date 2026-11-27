//! When the disk fills: a pure function of what Davy measured, so every
//! case has a test with fixed numbers.
//!
//! Logs of all projects share one index per day, so Davy cannot measure
//! one project's bytes on disk. The bound takes the growth of all logs
//! together and caps it with the longest log retention of any project, a
//! bound no split of the growth can exceed. When projects keep logs for
//! different times, the bound is therefore a worst case: at the current
//! rate, the disk fills on its day or later, or never. The estimate splits
//! the growth by each project's raw bytes and keeps each part as long as
//! its project does, which assumes every project's logs pack alike.
//!
//! The log growth comes from [`growth`]: raw bytes per day times how they
//! pack, or the disk sizes of the closed days. Error events add to SQLite
//! per project until each project's error retention balances, see
//! [`errors`]. Both run through [`fill`]. Days past `--hot-days` take
//! their cold size, see [`tiers`].

mod errors;
mod estimate;
mod fill;
mod growth;
mod tiers;

pub use errors::{ErrorBasis, Errors, Lean, MIN_EVENTS, ProjectErrors, StoreDay};
pub use fill::{Fill, HORIZON_DAYS};
pub use growth::{Basis, Confidence, LOOK_BACK, Logs, RawDay, Spread, hours};
pub use tiers::{PRIOR_PPM as COLD_PRIOR_PPM, Tiers};

use crate::usage::footprint::Prior;
use fill::Part;
use jiff::{ToSpan as _, civil::Date};
use rmcp::schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Closed log days the growth is the median of.
pub const DAYS: usize = 7;
/// Errors that add more than this of what the bound adds, or of the room
/// left to the limit where it never fills, in parts per million, lower a
/// high confidence while their bytes per event are not measured on 7
/// days: 5%. On errors alone, errors that began recently lower it once
/// they add more than this of the errors' growth.
const MEDIUM_SHARE_PPM: u64 = 50_000;
/// The share of the filesystem the forecast keeps free.
pub const HEADROOM_PERCENT: u64 = 10;
pub const HEADROOM_REASON: &str = "10% of the filesystem stays free: filesystems reserve blocks, and index merges and SQLite checkpoints need room for a copy of what they rewrite";

#[derive(Clone, Debug, PartialEq)]
pub struct Input {
    pub today: Date,
    /// Each closed log day on disk and the size of its index, oldest
    /// first.
    pub closed: Vec<(Date, u64)>,
    /// Each cold log day on disk and the size of its files, oldest first.
    pub cold: Vec<(Date, u64)>,
    /// Closed days the server keeps hot, `--hot-days`: all when `None`.
    pub hot_days: Option<u32>,
    /// What Davy's files take now.
    pub davy: u64,
    /// What SQLite, uploads and other files take now. SQLite grows from
    /// here by what `errors` add.
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
    /// SQLite's size once a day from `store_days`, the newest
    /// [`LOOK_BACK`] days and the sample before, oldest first.
    pub store: Vec<StoreDay>,
    /// What each project received and keeps of errors.
    pub errors: Vec<ProjectErrors>,
    /// How logs and events pack where the server has not measured it.
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
    /// What error events add to SQLite, `None` when no project received
    /// any.
    pub errors: Option<Errors>,
    /// The longest log retention of any project, which caps the growth.
    pub longest_days: u32,
    /// The shortest log retention of any project. Below `longest_days`,
    /// the forecast is a worst case: see [`Forecast::worst_case`].
    pub shortest_days: u32,
    /// The most Davy takes once retention deletes as much as arrives: the
    /// longest log retention's closed days and today, each a day's growth,
    /// or its cold share past `--hot-days`, SQLite with each project's
    /// error retention full, and uploads.
    pub steady_state: u64,
    /// How logs split into hot and cold days.
    pub tiers: Tiers,
    pub headroom: u64,
    /// `None` when the disk never fills at this rate, or fills beyond
    /// [`HORIZON_DAYS`].
    pub days_left: Option<u64>,
    /// The UTC day of `days_left`.
    pub full_on: Option<Date>,
    /// Whether the disk fills, but beyond [`HORIZON_DAYS`].
    pub beyond_10_years: bool,
    /// Why the forecast cannot show what a what-if for one project does,
    /// when it cannot.
    pub unseen: Option<String>,
    /// Where the growth comes from. `None` for a what-if of the bytes per
    /// day, which states the growth.
    pub logs: Option<Logs>,
    /// `full_on` at the high growth and at the low growth of logs in the
    /// window, `None` for never or beyond 10 years: the earliest and the
    /// latest day.
    pub full_on_range: [Option<Date>; 2],
    /// Each project's logs kept as long as it keeps them, at the middle
    /// growth. `None` for a what-if of the bytes per day, which Davy
    /// cannot split by project.
    pub estimate: Option<Estimate>,
}

/// When the disk fills by the estimate: never earlier than the bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Estimate {
    pub steady_state: u64,
    pub fill: Fill,
    /// Whether its sums put the day earlier than the bound's, so it took
    /// the bound's day: its ratio puts less on disk than the walk found.
    pub clamped: bool,
}

impl Forecast {
    /// Whether projects keep logs for different times. The forecast then
    /// takes all logs as kept the longest of them, so `days_left` is the
    /// fewest days there can be at the current rate of logs: the disk
    /// fills that day or later, or never.
    pub fn worst_case(&self) -> bool {
        self.shortest_days < self.longest_days && !self.errors_only()
    }

    /// Whether logs add nothing and the forecast rests on errors alone:
    /// log retentions then hold no growth to cap.
    pub fn errors_only(&self) -> bool {
        self.logs
            .as_ref()
            .is_some_and(|l| matches!(l.basis, Basis::ErrorsOnly { .. }))
    }

    /// Whether the forecast rests on errors alone while logs that arrived
    /// are too few or too new for a log basis: it may then fill earlier.
    pub fn logs_pending(&self) -> bool {
        self.logs.as_ref().is_some_and(|l| {
            matches!(
                l.basis,
                Basis::ErrorsOnly {
                    logs_pending: true,
                    ..
                }
            )
        })
    }

    /// What the estimate takes that the bound does not, in a phrase: each
    /// project's own retention where they differ, else only the middle
    /// growth.
    pub fn estimate_takes(&self) -> &'static str {
        if self.errors_only() {
            "at the middle bytes per event of errors"
        } else if self.worst_case() {
            "with each project's logs kept as long as it keeps them and packing alike"
        } else {
            "at the middle growth of logs"
        }
    }

    /// How far the answer can be trusted, with `partial` when the disk
    /// walk left something out. `None` where a what-if states the growth.
    pub fn confidence(&self, partial: bool) -> Option<Confidence> {
        let logs = self.logs.as_ref()?.confidence(partial);
        Some(match logs {
            Confidence::High if self.errors_unmeasured() => Confidence::Medium,
            c => c,
        })
    }

    /// One sentence on the basis, ending with why the confidence is lower
    /// than the logs alone make it.
    pub fn explain(&self, partial: bool) -> Option<String> {
        let logs = self.logs.as_ref()?;
        let why = logs.explain(partial);
        let errors = self
            .errors
            .filter(|_| logs.confidence(partial) == Confidence::High && self.errors_unmeasured());
        Some(match errors {
            Some(e) => format!(
                "{why}; confidence is medium as errors add over 5% of what the bound adds until the disk fills, or of the room left to the limit within 10 years where it never fills, and SQLite growth measured their bytes per event on {} of 7 days",
                e.own_days
            ),
            None => why,
        })
    }

    /// Errors add over 5% of what the bound adds, see
    /// [`Errors::share_ppm`], and fewer than 7 days measured their bytes
    /// per event.
    fn errors_unmeasured(&self) -> bool {
        self.errors
            .is_some_and(|e| e.own_days < DAYS && e.share_ppm > MEDIUM_SHARE_PPM)
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
    let found = errors::errors(
        &input.errors,
        &input.store,
        input.prior,
        input.today,
        input.now,
    );
    let logs = match what_if.logs_bytes_per_day {
        Some(_) => None,
        None => Some(log_growth(
            input,
            shortest.unwrap_or(input.default_days),
            found.as_ref(),
        )?),
    };
    let (growth, range) = match (&logs, what_if.logs_bytes_per_day) {
        (Some(l), _) => (l.bound(), l.growth),
        (None, bytes) => (bytes.unwrap_or(0), Spread::flat(bytes.unwrap_or(0))),
    };
    let headroom = input.total / 100 * HEADROOM_PERCENT;
    let limit = input.total.saturating_sub(headroom);
    let none = Vec::new();
    let (bound_errors, estimate_errors) = found
        .as_ref()
        .map_or((&none, &none), |f| (&f.bound, &f.estimate));
    let freed = freed(input, longest);
    let tiers = Tiers::of(input);
    let run = |growth: u64, errors: &[Part]| {
        bound(
            input,
            growth,
            tiers.steady(growth, longest),
            freed,
            errors,
            limit,
        )
    };
    let b = run(growth, bound_errors);
    let fill = Fill::new(input.today, b.days);
    let at = |growth, errors: &[Part]| Fill::new(input.today, run(growth, errors).days).full_on;
    // Logs that add nothing have no range of their own: the errors' bytes
    // per event of the bound and of the estimate give it.
    let full_on_range = match logs.as_ref().map(|l| l.basis) {
        Some(Basis::ErrorsOnly { .. }) => [at(0, bound_errors), at(0, estimate_errors)],
        _ => [at(range.high, bound_errors), at(range.low, bound_errors)],
    };
    let split = logs
        .as_ref()
        .map(|l| estimate::logs(input, l, &after, (longest, tiers)));
    let by_project = split.as_ref().is_some_and(|s| s.by_project);
    let estimate = split.map(|s| estimate(input, s, estimate_errors, limit, b.days));
    let errors = found.as_ref().map(|f| Errors {
        share_ppm: share_ppm(&b.parts, b.days, b.left),
        ..f.errors
    });
    Ok(Forecast {
        growth,
        errors,
        longest_days: longest,
        shortest_days: after.iter().map(|(_, n)| *n).min().unwrap_or(longest),
        steady_state: b.steady_state,
        tiers,
        headroom,
        days_left: fill.days_left,
        full_on: fill.full_on,
        beyond_10_years: fill.beyond_10_years,
        unseen: match &logs {
            Some(l)
                if what_if.logs_days.is_some() && matches!(l.basis, Basis::ErrorsOnly { .. }) =>
            {
                Some(LOGS_AS_0.to_owned())
            }
            _ => unseen(input, what_if, &after, longest, by_project),
        },
        logs,
        full_on_range,
        estimate,
    })
}

/// The bound: its steady state, its days left, `None` for never, its
/// parts, logs first, and the room left to the limit.
struct Bound {
    steady_state: u64,
    days: Option<u64>,
    parts: Vec<Part>,
    left: u64,
}

/// The bound at `growth` of logs that hold `logs` once retention
/// balances, with the errors' parts `errors`, once a what-if's shorter
/// retention deleted the closed days that take `freed`. Retention of N
/// days deletes a day once it is more than N days old, so the disk holds
/// N closed days and today, see [`Tiers::steady`].
fn bound(input: &Input, growth: u64, logs: u64, freed: u64, errors: &[Part], limit: u64) -> Bound {
    let now = input.davy.saturating_sub(input.fixed).saturating_sub(freed);
    let mut parts = vec![Part {
        growth,
        room: logs.saturating_sub(now),
    }];
    parts.extend_from_slice(errors);
    let steady_state = errors
        .iter()
        .fold(logs.saturating_add(input.fixed), |sum, p| {
            sum.saturating_add(p.room)
        });
    let used = input.used.saturating_sub(freed);
    Bound {
        steady_state,
        days: fill::days_left(used, limit, &parts),
        parts,
        left: limit.saturating_sub(used),
    }
}

/// What the closed days, hot and cold, take that a what-if lowering the
/// longest log retention to `longest` deletes at the next sweep, by their
/// sizes on disk: those older than `longest` days. Without such a
/// what-if, none.
fn freed(input: &Input, longest: u32) -> u64 {
    let before = input.projects.iter().map(|(_, n)| *n).max();
    if longest >= before.unwrap_or(input.default_days) {
        return 0;
    }
    let first = input
        .today
        .checked_sub(i64::from(longest).days())
        .unwrap_or(Date::MIN);
    input
        .closed
        .iter()
        .chain(&input.cold)
        .filter(|(day, _)| *day < first)
        .fold(0, |sum, (_, bytes)| sum.saturating_add(*bytes))
}

/// What the error parts, all of `parts` but the first, add, in parts per
/// million, rounded up, each part at most its room: until day `days`, of
/// what all parts add by then. Where the disk never fills, within
/// [`HORIZON_DAYS`], of `left`, the room to the limit, as logs that hold
/// their retention add nothing and would make any errors all of it.
fn share_ppm(parts: &[Part], days: Option<u64>, left: u64) -> u64 {
    let t = days.unwrap_or(HORIZON_DAYS);
    let add = |p: &Part| u128::from(p.growth.saturating_mul(t).min(p.room));
    let errors: u128 = parts.iter().skip(1).map(add).sum();
    let all = days.map_or(u128::from(left), |_| parts.iter().map(add).sum());
    u64::try_from((errors * 1_000_000).div_ceil(all.max(1))).unwrap_or(u64::MAX)
}

/// The estimate from the log parts `split` and the errors' parts
/// `errors`, never earlier than the bound's `bound` days, `None` for
/// never. Where its sums put the day earlier, as when its ratio puts less
/// on disk now than the walk found, it takes the bound's day.
fn estimate(
    input: &Input,
    split: estimate::Split,
    errors: &[Part],
    limit: u64,
    bound: Option<u64>,
) -> Estimate {
    let steady_state = errors
        .iter()
        .fold(split.steady.saturating_add(input.fixed), |sum, p| {
            sum.saturating_add(p.room)
        });
    let mut parts = split.parts;
    parts.extend_from_slice(errors);
    // A shorter retention deletes its older days at the next sweep.
    let used = input.used.saturating_sub(split.freed);
    let sums = fill::days_left(used, limit, &parts);
    let (days, clamped) = match (sums, bound) {
        (Some(e), Some(b)) => (Some(e.max(b)), e < b),
        (Some(_), None) => (None, true),
        (None, _) => (None, false),
    };
    Estimate {
        steady_state,
        fill: Fill::new(input.today, days),
        clamped,
    }
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

/// What a what-if of a log retention does to a forecast on errors alone,
/// which takes no log retention: only the closed log days it deletes
/// leave the disk.
const LOGS_AS_0: &str = "this forecast counts logs as 0, as it rests on errors alone, so a log retention changes it only by the log days on disk it deletes";

/// Why the what-if for one project leaves the bound as it is, when it
/// does: another project keeps logs `longest` days with it and without
/// it. The bound already takes the project's logs as kept that long, and
/// Davy cannot tell the project's share of the growth on disk. The
/// estimate shows it `by_project`, where Davy counted raw bytes per
/// project.
fn unseen(
    input: &Input,
    what_if: &WhatIf,
    after: &[(&str, u32)],
    longest: u32,
    by_project: bool,
) -> Option<String> {
    let project = what_if.project.as_deref()?;
    let now = input.projects.iter().find(|(s, _)| s == project)?.1;
    if what_if.logs_days == Some(now) {
        return None;
    }
    let before = input.projects.iter().map(|(_, n)| *n).max()?;
    let (holder, _) = after
        .iter()
        .find(|(slug, n)| *slug != project && *n == longest)?;
    let estimate = if by_project {
        "The estimate shows it: it splits the growth by each project's raw bytes and assumes every project's logs pack alike"
    } else {
        "Nor can the estimate: it splits the growth by each project's raw bytes, and Davy has counted none"
    };
    (before == longest).then(|| {
        format!(
            "the worst case cannot show this what-if: it takes the logs of every project as kept {longest} days, as long as {holder} keeps them, and logs of all projects share one index per day, so Davy cannot tell how much of the disk is {project}'s. {estimate}"
        )
    })
}

/// The log growth. Logs add nothing where Davy counted no raw log bytes
/// and the direct method finds no closed day, and [`growth::logs`] has no
/// forecast where logs are too few or too new. Either way a growth of 0
/// where errors grow in `found`, which with logs that arrived can only
/// understate, so its confidence is low. Without errors there is no
/// forecast. A server with a log basis gets what it got before.
fn log_growth(
    input: &Input,
    shortest: u32,
    found: Option<&errors::Found>,
) -> Result<Logs, NoForecast> {
    let first = first_uncut(input, shortest);
    let day = input
        .closed
        .iter()
        .any(|(day, _)| *day >= first && *day < input.today);
    let pending = if day || growth::counted_bytes(input) > 0 {
        match growth::logs(input, measured(input, shortest)) {
            Err(NoForecast::History(why)) => Some(why),
            logs => return logs,
        }
    } else {
        None
    };
    match (found, pending) {
        (Some(f), pending) => Ok(growth::errors_only(
            input,
            f.lean,
            first,
            pending.as_deref(),
        )),
        (None, Some(why)) => Err(NoForecast::History(why)),
        (None, None) => Err(NoForecast::History(format!(
            "nothing grows yet: Davy counted no raw log bytes in the last 30 days, the disk holds no closed log day since {first}, and no project received error events in the last 7 days"
        ))),
    }
}

/// The first day no retention has cut: within the `shortest` retention of
/// any project.
fn first_uncut(input: &Input, shortest: u32) -> Date {
    input
        .today
        .checked_sub(i64::from(shortest).days())
        .unwrap_or(Date::MIN)
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
    let first = first_uncut(input, shortest);
    let days: Vec<&(Date, u64)> = input
        .closed
        .iter()
        .rev()
        .filter(|(day, _)| *day >= first && *day < input.today)
        .take(DAYS)
        .collect();
    let sizes: Vec<u64> = days.iter().map(|(_, bytes)| *bytes).collect();
    // With 1 day of retention, yesterday is the only day not cut, and
    // with 1 hot day the only one not cold.
    let hot = input.hot_days.map_or(shortest, |h| h.min(shortest));
    let need = if hot >= 2 { 2 } else { 1 };
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

#[cfg(test)]
#[path = "forecast_errors_only_tests.rs"]
mod errors_only_tests;

#[cfg(test)]
#[path = "forecast_pending_tests.rs"]
mod pending_tests;
