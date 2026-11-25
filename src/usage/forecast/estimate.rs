//! The estimate's logs: one part per project, each its share of the raw
//! bytes times one ratio for all, kept as long as that project keeps logs.
//! Projects' logs do not all pack alike, so this is an estimate, not a
//! bound: nginx and syslog lines differ by almost 2 times in the footprint
//! benchmark.

use super::{
    DAYS, Input, LOOK_BACK, Logs,
    fill::Part,
    growth::{Basis, raw_per_day, whole},
};
use crate::time::DAY_NS;
use jiff::{ToSpan as _, civil::Date};

/// The parts of the logs, what they hold once retention deletes as much
/// as arrives, what a what-if's shorter retention frees now, and whether
/// they are split by project: without raw bytes per project, they are
/// one part.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Split {
    pub parts: Vec<Part>,
    pub steady: u64,
    pub freed: u64,
    pub by_project: bool,
}

/// The log parts of `input` with each project keeping logs as `after`
/// says. A project's growth is the basis' middle growth times its share
/// of the raw bytes per day, which is its raw bytes times one ratio: the
/// middle growth over all raw bytes per day. Its room is what it holds
/// once retention balances, less what it holds now: its uncut days within
/// that retention whose directory is on disk. Without raw bytes per
/// project, the logs are one part kept `longest` days, which frees what
/// the bound frees.
pub fn logs(input: &Input, logs: &Logs, after: &[(&str, u32)], longest: u32) -> Split {
    let growth = logs.growth.mid;
    let shares = shares(input, logs);
    let total: u128 = shares.iter().map(|(_, r)| u128::from(*r)).sum();
    if total == 0 {
        let steady = growth.saturating_mul(u64::from(longest) + 1);
        let freed = super::freed(input, longest);
        let now = input.davy.saturating_sub(input.fixed).saturating_sub(freed);
        return Split {
            parts: vec![Part {
                growth,
                room: steady.saturating_sub(now),
            }],
            steady,
            freed,
            by_project: false,
        };
    }
    let disk = |raw: u64| clamp(u128::from(raw) * u128::from(growth) / total);
    let mut split = Split {
        by_project: true,
        ..Split::default()
    };
    for (slug, days) in after {
        let raw = shares
            .iter()
            .find(|(s, _)| s == slug)
            .map_or(0, |(_, r)| *r);
        let per_day = disk(raw);
        let steady = per_day.saturating_mul(u64::from(*days) + 1);
        let held = disk(on_disk(input, slug, *days));
        let kept = input.projects.iter().find(|(s, _)| s == slug);
        let before = kept.map_or(held, |(_, n)| disk(on_disk(input, slug, *n)));
        split.freed = split.freed.saturating_add(before.saturating_sub(held));
        split.steady = split.steady.saturating_add(steady);
        split.parts.push(Part {
            growth: per_day,
            room: steady.saturating_sub(held),
        });
    }
    split
}

/// Each project's raw bytes per day: over the newest 7 whole closed days,
/// the larger of the median and the mean, as for all projects together.
/// On a first day, its bytes over the time counted.
fn shares<'a>(input: &'a Input, logs: &Logs) -> Vec<(&'a str, u64)> {
    let first = input
        .today
        .checked_sub(LOOK_BACK.days())
        .unwrap_or(Date::MIN);
    let slugs = input.projects.iter().map(|(s, _)| s.as_str());
    if logs.basis == Basis::FirstDay {
        let counted = u128::from(logs.counted_ns.unwrap_or(DAY_NS).unsigned_abs().max(1));
        return slugs
            .map(|slug| {
                let bytes = raw_of(input, slug, |day| day >= first);
                let per_day = u128::from(bytes) * u128::from(DAY_NS.unsigned_abs()) / counted;
                (slug, clamp(per_day))
            })
            .collect();
    }
    let days: Vec<Date> = (1..=LOOK_BACK)
        .filter_map(|i| input.today.checked_sub(i.days()).ok())
        .filter(|day| whole(input, *day))
        .take(DAYS)
        .collect();
    slugs
        .map(|slug| {
            let raws: Vec<u64> = days
                .iter()
                .map(|day| raw_of(input, slug, |d| d == *day))
                .collect();
            (slug, raw_per_day(&raws).map_or(0, |r| r.mid))
        })
        .collect()
}

/// Raw bytes of `slug` that sit on disk while it keeps logs `days` days:
/// its uncut rows from `days` days before today on, of a day whose
/// directory is on disk. Today's and tomorrow's directories are open.
fn on_disk(input: &Input, slug: &str, days: u32) -> u64 {
    let first = input
        .today
        .checked_sub(i64::from(days).days())
        .unwrap_or(Date::MIN);
    let on_disk = |day: Date| day >= input.today || input.closed.iter().any(|(d, _)| *d == day);
    input
        .raw
        .iter()
        .filter(|r| r.day >= first && on_disk(r.day))
        .flat_map(|r| &r.projects)
        .filter(|(s, _, cut)| s == slug && !cut)
        .fold(0_u64, |sum, (_, b, _)| sum.saturating_add(*b))
}

/// Raw bytes of `slug` on the days `keep` takes, cut or not.
fn raw_of(input: &Input, slug: &str, keep: impl Fn(Date) -> bool) -> u64 {
    input
        .raw
        .iter()
        .filter(|r| keep(r.day))
        .flat_map(|r| &r.projects)
        .filter(|(s, _, _)| s == slug)
        .fold(0_u64, |sum, (_, b, _)| sum.saturating_add(*b))
}

fn clamp(n: u128) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

#[cfg(test)]
#[path = "estimate_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "estimate_errors_tests.rs"]
mod errors_tests;
