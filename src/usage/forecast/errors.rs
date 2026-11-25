//! How many bytes error events add to SQLite per day, per project, and how
//! much more each project adds before its error retention deletes as much
//! as arrives. Events per day come from what the project received. Bytes
//! per event come from SQLite's daily growth where enough events arrived,
//! and from the footprint benchmark until 7 days measured it.

use super::{DAYS, LOOK_BACK, fill::Part, growth::spread};
use crate::{
    time::{self, DAY_NS, HOUR_NS},
    usage::{Count, footprint::Prior},
};
use jiff::{ToSpan as _, civil::Date};

/// New events a day of SQLite growth needs to measure bytes per event.
/// Below them, sessions, check-ins and pages freed by retention weigh too
/// much against the events.
pub const MIN_EVENTS: u64 = 10_000;
/// The longest gap between two samples of SQLite's size that still makes
/// a day: a server down for longer gives no day.
const MAX_GAP_MS: i64 = 2 * DAY_NS / 1_000_000;
const PPM: u128 = 1_000_000;

/// SQLite's size and the events it kept, sampled once a UTC day.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StoreDay {
    pub day: Date,
    pub at_ms: i64,
    /// What its live pages take.
    pub bytes: u64,
    /// The events kept.
    pub events: u64,
}

/// What one project received and keeps of errors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectErrors {
    pub slug: String,
    /// When the project was made, ns: it received nothing before.
    pub created: i64,
    /// The first day `usage_daily` holds error events of it: Davy counted
    /// none before.
    pub first: Date,
    /// Error events received and their bytes as sent, per accept day,
    /// from 7 days before today on.
    pub daily: Vec<(Date, Count)>,
    /// The events it keeps now.
    pub stored: u64,
    /// The days it keeps errors.
    pub days: u32,
}

/// Where bytes per event come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorBasis {
    /// 7 days of SQLite growth on this server.
    Measured,
    /// 1 to 6 such days, and the footprint benchmark for the rest.
    Blended,
    /// The footprint benchmark alone.
    Footprint,
}

impl ErrorBasis {
    pub fn name(self) -> &'static str {
        match self {
            Self::Measured => "measured",
            Self::Blended => "blended",
            Self::Footprint => "footprint",
        }
    }
}

/// The error growth of all projects together.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Errors {
    /// Projects that received error events.
    pub projects: usize,
    pub events_per_day: u64,
    /// SQLite bytes per event of the estimate, over all projects:
    /// `estimate_growth` over `events_per_day`, rounded down.
    pub bytes_per_event: u64,
    /// Days of SQLite growth that measured bytes per event, `k`.
    pub own_days: usize,
    pub basis: ErrorBasis,
    /// SQLite bytes per day the bound adds, at each project's highest of
    /// its benchmark, the measured high end and that end scaled as
    /// [`blend`] scales the median: never below `estimate_growth`.
    pub growth: u64,
    /// SQLite bytes per day the estimate adds.
    pub estimate_growth: u64,
    /// What the bound's error parts add, in parts per million, rounded
    /// up, each at most its room: of what all its parts add until the
    /// bound's day, or, when the disk never fills, within 10 years of the
    /// room left to the limit.
    pub share_ppm: u64,
}

/// The error growth, and its parts for the bound and the estimate: one per
/// project that received events.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub errors: Errors,
    pub bound: Vec<Part>,
    pub estimate: Vec<Part>,
}

/// The error growth of `errors`, or `None` when no project received an
/// event: then it adds nothing, and SQLite stays as it is.
pub fn errors(
    errors: &[ProjectErrors],
    store: &[StoreDay],
    prior: Prior,
    today: Date,
    now: i64,
) -> Option<Found> {
    let own = per_event(store, today);
    let mut found = Found {
        errors: Errors {
            projects: 0,
            events_per_day: 0,
            bytes_per_event: 0,
            own_days: own.days,
            basis: match own.days {
                0 => ErrorBasis::Footprint,
                k if k >= DAYS => ErrorBasis::Measured,
                _ => ErrorBasis::Blended,
            },
            growth: 0,
            estimate_growth: 0,
            share_ppm: 0,
        },
        bound: Vec::new(),
        estimate: Vec::new(),
    };
    let rates: Vec<(&ProjectErrors, u64, u64)> = errors
        .iter()
        .filter_map(|p| {
            let (per_day, count) = rate(p, today, now);
            (per_day > 0).then(|| (p, per_day, prior_bytes(prior, count)))
        })
        .collect();
    let mean = mean_prior(&rates);
    for (p, per_day, prior) in rates {
        let mid = blend(own, prior, mean);
        let high = own
            .high
            .map_or(prior, |h| h.max(prior).max(scaled(h, prior, mean)));
        let (bound, estimate) = (
            part(per_day, high, p.days, p.stored),
            part(per_day, mid, p.days, p.stored),
        );
        let e = &mut found.errors;
        e.projects += 1;
        e.events_per_day = e.events_per_day.saturating_add(per_day);
        e.growth = e.growth.saturating_add(bound.growth);
        e.estimate_growth = e.estimate_growth.saturating_add(estimate.growth);
        found.bound.push(bound);
        found.estimate.push(estimate);
    }
    let e = &mut found.errors;
    e.bytes_per_event = e.estimate_growth / e.events_per_day.max(1);
    (e.projects > 0).then_some(found)
}

/// Events per day a project received and the count they come from: the
/// mean over the closed days of the newest 7 since its first errors, as
/// errors come in bursts. They begin on the first day `usage_daily` holds
/// them, or when the project was made if that is later. Until a whole day
/// has closed since, it counts up to now, over at least an hour.
pub fn rate(p: &ProjectErrors, today: Date, now: i64) -> (u64, Count) {
    let week = today.checked_sub(7.days()).map_or(i64::MIN, time::midnight);
    let start = week.max(p.created).max(time::midnight(p.first));
    let end = time::midnight(today);
    let first = time::day_of(start);
    let (last, span) = if end.saturating_sub(start) >= DAY_NS {
        (Some(today), end - start)
    } else {
        (None, now.saturating_sub(start).max(HOUR_NS))
    };
    let count = p
        .daily
        .iter()
        .filter(|(day, _)| *day >= first && last.is_none_or(|l| *day < l))
        .fold(Count::default(), |sum, (_, c)| Count {
            records: sum.records.saturating_add(c.records),
            bytes: sum.bytes.saturating_add(c.bytes),
        });
    let per_day = (u128::from(count.records) * u128::from(DAY_NS.unsigned_abs()))
        .div_ceil(u128::from(span.unsigned_abs()));
    (clamp(per_day), count)
}

/// SQLite bytes of an event of the size `count` averages, as the benchmark
/// measured it with every payload kept, rounded up. Sampling keeps fewer
/// payloads of an issue with many events.
pub fn prior_bytes(prior: Prior, count: Count) -> u64 {
    let payload = (u128::from(count.bytes) * u128::from(prior.payload_ppm))
        .div_ceil(u128::from(count.records.max(1)) * PPM);
    prior.event_bytes.saturating_add(clamp(payload))
}

/// Bytes per event measured on this server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PerEvent {
    /// The median of the measured days.
    pub own: Option<u64>,
    /// Their high end: with 5 or more the second highest, else the most.
    pub high: Option<u64>,
    /// How many days measured it, at most 7.
    pub days: usize,
}

/// Bytes per event from the newest 7 days of the newest [`LOOK_BACK`] when
/// SQLite's live pages grew while the events kept grew by
/// [`MIN_EVENTS`] or more, each day being two samples at most 48 hours
/// apart. Pages that retention freed are reused, so a day measures the
/// net growth.
pub fn per_event(store: &[StoreDay], today: Date) -> PerEvent {
    let first = today.checked_sub(LOOK_BACK.days()).unwrap_or(Date::MIN);
    let values: Vec<u64> = store
        .windows(2)
        .rev()
        .filter(|w| w[1].day >= first)
        .filter_map(|w| {
            let gap = w[1].at_ms.saturating_sub(w[0].at_ms);
            let events = w[1].events.saturating_sub(w[0].events);
            let bytes = w[1].bytes.checked_sub(w[0].bytes)?;
            (gap > 0 && gap <= MAX_GAP_MS && events >= MIN_EVENTS).then(|| bytes / events)
        })
        .take(DAYS)
        .collect();
    let s = spread(&values);
    PerEvent {
        own: s.map(|s| s.mid),
        high: s.map(|s| s.high),
        days: values.len(),
    }
}

/// The benchmark's bytes per event over all projects, each weighed by its
/// events per day: what the server's measured days stand for.
fn mean_prior(rates: &[(&ProjectErrors, u64, u64)]) -> u64 {
    let events: u128 = rates.iter().map(|(_, n, _)| u128::from(*n)).sum();
    let bytes: u128 = rates
        .iter()
        .map(|(_, n, b)| u128::from(*n) * u128::from(*b))
        .sum();
    clamp(bytes / events.max(1))
}

/// `k` days measured weighed against the benchmark's `prior` for the rest
/// of 7. SQLite grows by the events of all projects together, so what it
/// measured is scaled by the project's `prior` over the `mean` of all:
/// a project with larger payloads takes more of it.
pub fn blend(own: PerEvent, prior: u64, mean: u64) -> u64 {
    let k = u128::try_from(own.days.min(DAYS)).unwrap_or(0);
    let all = u128::try_from(DAYS).unwrap_or(7);
    let measured = u128::from(scaled(own.own.unwrap_or(0), prior, mean));
    let sum = k * measured + (all - k) * u128::from(prior);
    clamp(sum / all)
}

/// Measured `bytes` per event of all projects for a project whose
/// benchmark is `prior`, of the `mean` benchmark, rounded down.
fn scaled(bytes: u64, prior: u64, mean: u64) -> u64 {
    clamp(u128::from(bytes) * u128::from(prior) / u128::from(mean.max(1)))
}

/// A project's part: `per_day` events of `bytes` each, until it keeps
/// `days` days of them. One that keeps that many already adds nothing.
pub fn part(per_day: u64, bytes: u64, days: u32, stored: u64) -> Part {
    let steady = u128::from(per_day) * u128::from(days);
    let more = steady.saturating_sub(u128::from(stored));
    Part {
        growth: clamp(u128::from(per_day) * u128::from(bytes)),
        room: clamp(more.saturating_mul(u128::from(bytes))),
    }
}

fn clamp(n: u128) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

#[cfg(test)]
#[path = "errors_tests.rs"]
mod tests;
