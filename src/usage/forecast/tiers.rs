//! Logs in two tiers. A day older than `--hot-days` turns into cold
//! files that take a share of what its index took: the cold ratio. Logs
//! kept N days therefore hold today and the hot closed days at the
//! growth, and the other closed days at the growth times that ratio.
//!
//! The ratio is the server's own where it holds days of both tiers: the
//! median of disk bytes per raw byte over its cold days, divided by that
//! over its hot closed days, each day whole, uncut and at least
//! [`MIN_RAW`]. Elsewhere it is [`PRIOR_PPM`], the largest the storage
//! benchmark measured with a margin, see docs/storage-benchmark.md.

use super::{
    Input, LOOK_BACK,
    growth::{MIN_RAW, whole},
    median,
};
use jiff::{ToSpan as _, civil::Date};

/// Cold bytes per hot byte where the server has not measured it, in
/// parts per million: 0.2.
pub const PRIOR_PPM: u64 = 200_000;
const PPM: u128 = 1_000_000;

/// How the days of logs split into hot and cold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tiers {
    /// Closed days kept hot, every one when `None`.
    pub hot_days: Option<u32>,
    /// Cold bytes per hot byte, in parts per million, at most 1.
    pub ratio_ppm: u64,
    /// Whether the ratio is the server's own.
    pub own: bool,
}

impl Tiers {
    /// Every day hot, for tests of what does not split the tiers.
    #[cfg(test)]
    pub const ALL_HOT: Self = Self {
        hot_days: None,
        ratio_ppm: PRIOR_PPM,
        own: false,
    };

    pub fn of(input: &Input) -> Self {
        let own = own(input);
        Self {
            hot_days: input.hot_days,
            ratio_ppm: own.unwrap_or(PRIOR_PPM),
            own: own.is_some(),
        }
    }

    /// Of `days` closed days, those kept hot.
    pub fn hot(&self, days: u32) -> u32 {
        self.hot_days.map_or(days, |h| h.min(days))
    }

    /// What logs that take `growth` a day hot take kept `days` days: today
    /// and the hot closed days whole, the rest at the cold ratio.
    pub fn steady(&self, growth: u64, days: u32) -> u64 {
        let hot = self.hot(days);
        let cold = u64::from(days - hot);
        growth
            .saturating_mul(u64::from(hot) + 1)
            .saturating_add(self.cold(growth).saturating_mul(cold))
    }

    /// What `bytes` of a hot day take once cold.
    pub fn cold(&self, bytes: u64) -> u64 {
        clamp(u128::from(bytes) * u128::from(self.ratio_ppm) / PPM)
    }
}

/// The server's own ratio, if it holds days of both tiers to measure.
fn own(input: &Input) -> Option<u64> {
    let hot = median(&per_raw(input, &input.closed))?;
    let cold = median(&per_raw(input, &input.cold))?;
    let ratio = u128::from(cold) * PPM / u128::from(hot.max(1));
    Some(clamp(ratio).min(1_000_000))
}

/// Disk bytes per raw byte, in ppm, of each day of `sizes` in the newest
/// [`LOOK_BACK`] that is whole, uncut and at least [`MIN_RAW`].
fn per_raw(input: &Input, sizes: &[(Date, u64)]) -> Vec<u64> {
    let first = input
        .today
        .checked_sub(LOOK_BACK.days())
        .unwrap_or(Date::MIN);
    sizes
        .iter()
        .filter(|(day, _)| *day >= first && *day < input.today && whole(input, *day))
        .filter_map(|(day, bytes)| {
            let rows = input.raw.iter().filter(|r| r.day == *day);
            let (raw, cut) = rows
                .flat_map(|r| &r.projects)
                .fold((0_u64, false), |(raw, cut), (_, b, c)| {
                    (raw.saturating_add(*b), cut || *c)
                });
            (!cut && raw >= MIN_RAW).then(|| clamp(u128::from(*bytes) * PPM / u128::from(raw)))
        })
        .collect()
}

fn clamp(n: u128) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

#[cfg(test)]
#[path = "tiers_tests.rs"]
mod tests;
