//! How many bytes of logs reach the disk per day. The growth is the raw
//! bytes Davy counts per day times how they pack on disk, or the size on
//! disk of the days that measured how they pack. Raw bytes are counted
//! from the first hour; the packing is measured on each closed day the
//! server holds whole and blended with the footprint benchmark until there
//! are enough such days.

use super::{DAYS, Direct, Input, NoForecast, median};
use crate::{
    time::{self, DAY_NS, HOUR_NS},
    usage::footprint::{Band, Prior},
};
use jiff::{ToSpan as _, civil::Date};

/// Closed days back from today a day may count for the ratio, so a server
/// quiet at weekends still fills its 7.
pub const LOOK_BACK: i64 = 30;
/// Raw bytes a day needs to count for the ratio. Below them the fixed cost
/// of a day index dominates.
pub const MIN_RAW: u64 = 10_000_000;
const PPM: u128 = 1_000_000;

/// Raw bytes of one index day from `log_days`: per project, its slug, the
/// bytes and whether retention cut the pair.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawDay {
    pub day: Date,
    pub projects: Vec<(String, u64, bool)>,
}

/// Where the growth comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Basis {
    /// As many whole closed days as retention lets the server hold, at
    /// most 7, measured how logs pack. The growth is their median size on
    /// disk, or raw bytes per day times their ratio when a newer day was
    /// quiet.
    Measured,
    /// The median size on disk of the closed days, without a ratio of the
    /// server's own: it counted raw bytes too late or too few.
    MeasuredDisk,
    /// Raw bytes per day times a ratio of the server's own days and the
    /// footprint benchmark.
    Blended,
    /// Raw bytes per hour of a server without a whole closed day, times
    /// the footprint benchmark.
    FirstDay,
}

impl Basis {
    pub fn name(self) -> &'static str {
        match self {
            Self::Measured => "measured",
            Self::MeasuredDisk => "measured_disk",
            Self::Blended => "blended",
            Self::FirstDay => "first_day",
        }
    }
}

/// A low, middle and high value of bytes per day.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Spread {
    pub low: u64,
    pub mid: u64,
    pub high: u64,
}

impl Spread {
    pub fn flat(n: u64) -> Self {
        Self {
            low: n,
            mid: n,
            high: n,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Confidence {
    Low,
    Medium,
    High,
}

impl Confidence {
    pub fn name(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// The log growth and what it rests on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Logs {
    pub basis: Basis,
    /// Disk bytes per day: the middle and the range of the window.
    pub growth: Spread,
    /// Closed days measured for the ratio, `n`.
    pub own_days: usize,
    /// The most closed days the server can hold uncut, at most 7.
    pub possible_days: usize,
    /// Raw bytes per day, when the growth rests on them or whole days
    /// give them.
    pub raw: Option<Spread>,
    /// Whole closed days `raw` is read from; 0 on a first day.
    pub raw_days: usize,
    /// How long Davy has counted raw bytes, on a first day.
    pub counted_ns: Option<i64>,
    /// Counting started inside the newest 7 days.
    pub recent: bool,
    /// Disk bytes per raw byte in ppm, where the growth rests on raw bytes
    /// or on days that measured it.
    pub ratio: Option<Band>,
    /// The median ratio of the server's own days.
    pub own_ratio: Option<u64>,
    pub prior: Prior,
    /// One sentence on the basis, to quote to a person.
    pub why: String,
}

impl Logs {
    /// The growth the bound and doctor decide on: the high growth of a
    /// blend or a first day, which lean on the benchmark or on hours, the
    /// middle where the server measured `possible` days of its own or the
    /// disk sizes alone.
    pub fn bound(&self) -> u64 {
        match self.basis {
            Basis::Blended | Basis::FirstDay => self.growth.high,
            Basis::Measured | Basis::MeasuredDisk => self.growth.mid,
        }
    }

    /// How far the answer can be trusted, with `partial` when the disk
    /// walk left something out.
    pub fn confidence(&self, partial: bool) -> Confidence {
        match self.basis {
            Basis::FirstDay => Confidence::Low,
            _ if self.lowered(partial).is_some() => Confidence::Low,
            Basis::Blended | Basis::MeasuredDisk => Confidence::Medium,
            Basis::Measured => Confidence::High,
        }
    }

    /// `why`, and why the confidence is low where the basis does not say.
    pub fn explain(&self, partial: bool) -> String {
        match self.lowered(partial) {
            Some(reason) => format!("{}; confidence is low {reason}", self.why),
            None => self.why.clone(),
        }
    }

    /// What lowers the confidence of the basis to low.
    fn lowered(&self, partial: bool) -> Option<&'static str> {
        if partial {
            Some("as the disk walk left something out")
        } else if self.recent && !matches!(self.basis, Basis::MeasuredDisk | Basis::FirstDay) {
            Some("while Davy has counted raw log bytes for under 7 whole days")
        } else {
            None
        }
    }
}

/// One closed day of the newest [`LOOK_BACK`].
struct Day {
    raw: u64,
    cut: bool,
    disk: Option<u64>,
    whole: bool,
}

impl Day {
    fn qualifies(&self) -> bool {
        self.whole && !self.cut && self.raw >= MIN_RAW && self.disk.is_some()
    }

    /// A whole day too quiet to measure how logs pack.
    fn quiet(&self) -> bool {
        self.whole && self.raw < MIN_RAW
    }

    /// Disk bytes per raw byte, in ppm.
    fn ratio(&self) -> u64 {
        let disk = u128::from(self.disk.unwrap_or(0));
        clamp(disk * PPM / u128::from(self.raw.max(1)))
    }
}

/// The log growth of `input`, given what the direct method of the disk
/// sizes measured or why it measured nothing.
pub fn logs(input: &Input, direct: Result<Direct, NoForecast>) -> Result<Logs, NoForecast> {
    let days = closed(input);
    let possible = possible(input);
    let own: Vec<(usize, &Day)> = days
        .iter()
        .enumerate()
        .filter(|(_, d)| d.qualifies())
        .take(possible)
        .collect();
    let ratios: Vec<u64> = own.iter().map(|(_, d)| d.ratio()).collect();
    let raws: Vec<u64> = days
        .iter()
        .filter(|d| d.whole)
        .take(DAYS)
        .map(|d| d.raw)
        .collect();
    let n = own.len();
    let base = Logs {
        basis: Basis::MeasuredDisk,
        growth: Spread::flat(0),
        own_days: n,
        possible_days: possible,
        raw: raw_per_day(&raws),
        raw_days: raws.len(),
        counted_ns: None,
        recent: input.counted_since.is_some_and(|c| {
            input
                .today
                .checked_sub(7.days())
                .is_ok_and(|d| c > time::midnight(d))
        }),
        ratio: None,
        own_ratio: median(&ratios),
        prior: input.prior,
        why: String::new(),
    };
    // A quiet day newer than the oldest own day: the server went quiet.
    let quiet = own
        .last()
        .is_some_and(|(oldest, _)| days[..*oldest].iter().any(Day::quiet));
    let whole_day = days.first().is_some_and(|d| d.whole);
    let counted = input.counted_since.map(|c| input.now.saturating_sub(c));
    let prior = input.prior.ratio;
    match direct {
        _ if n == possible => {
            let sizes: Vec<u64> = own.iter().filter_map(|(_, d)| d.disk).collect();
            Ok(measured(&base, &sizes, &ratios, prior, quiet))
        }
        Ok(d) if upgraded(input, &d) => {
            let whole = days.iter().filter(|d| d.whole).count();
            let why = if n == 0 && whole >= possible {
                SMALL.to_owned()
            } else {
                not_counted(input)
            };
            Ok(disk_only(&base, d, &why))
        }
        _ if n >= 1 => Ok(blended(&base, &ratios, prior)),
        _ => match (counted, counted_bytes(input)) {
            (Some(c), _) if !whole_day && c < HOUR_NS => Err(NoForecast::History(format!(
                "Davy has counted raw log bytes for {} minutes; a first forecast needs an hour of them, or closed log days on disk",
                c.max(0) / (HOUR_NS / 60)
            ))),
            (Some(c), bytes) if !whole_day && bytes > 0 => Ok(first_day(&base, input, c, bytes)),
            _ => direct.map(|d| disk_only(&base, d, SMALL)),
        },
    }
}

/// The closed days of the newest [`LOOK_BACK`], newest first.
fn closed(input: &Input) -> Vec<Day> {
    (1..=LOOK_BACK)
        .filter_map(|i| input.today.checked_sub(i.days()).ok())
        .map(|day| {
            let rows = input.raw.iter().filter(|r| r.day == day);
            let (raw, cut) = rows
                .flat_map(|r| &r.projects)
                .fold((0_u64, false), |(raw, cut), (_, bytes, c)| {
                    (raw.saturating_add(*bytes), cut || *c)
                });
            Day {
                raw,
                cut,
                disk: input
                    .closed
                    .iter()
                    .find(|(d, _)| *d == day)
                    .map(|(_, b)| *b),
                whole: whole(input, day),
            }
        })
        .collect()
}

/// At most 7, and no more than the shortest log retention of a project
/// with raw bytes: a server keeping logs 3 days never holds more than 3
/// closed days uncut.
fn possible(input: &Input) -> usize {
    let counted = input
        .projects
        .iter()
        .filter(|(slug, _)| recent(input).any(|r| r.projects.iter().any(|(s, _, _)| s == slug)));
    let shortest = counted
        .map(|(_, n)| *n)
        .min()
        .or_else(|| input.projects.iter().map(|(_, n)| *n).min())
        .unwrap_or(input.default_days);
    usize::try_from(shortest).map_or(DAYS, |n| n.clamp(1, DAYS))
}

/// Whether Davy counted all of `day`: it started at or after counting did.
pub(super) fn whole(input: &Input, day: Date) -> bool {
    input
        .counted_since
        .is_some_and(|c| time::midnight(day) >= c)
}

/// Whether the server ran a version that did not count raw bytes: it
/// never counted, or the direct method holds a closed day that ended
/// before counting started and has no `log_days` row, which only a
/// counting version writes. A line it stored on an older day, as a
/// replayed file does, writes one. The day counting started on is not
/// such a day: a new server holds it too.
fn upgraded(input: &Input, d: &Direct) -> bool {
    input.counted_since.is_none_or(|c| {
        input.closed.iter().any(|(day, _)| {
            *day >= d.oldest && *day < time::day_of(c) && !input.raw.iter().any(|r| r.day == *day)
        })
    })
}

/// Every raw byte counted on the index days of the newest [`LOOK_BACK`]
/// and on.
fn counted_bytes(input: &Input) -> u128 {
    recent(input)
        .flat_map(|r| &r.projects)
        .fold(0, |sum, (_, b, _)| sum + u128::from(*b))
}

/// The rows of `log_days` from [`LOOK_BACK`] days before today on: the
/// estimate reads older ones too, of logs a project keeps longer.
fn recent(input: &Input) -> impl Iterator<Item = &RawDay> {
    let first = input
        .today
        .checked_sub(LOOK_BACK.days())
        .unwrap_or(Date::MIN);
    input.raw.iter().filter(move |r| r.day >= first)
}

fn disk_only(base: &Logs, d: Direct, why: &str) -> Logs {
    Logs {
        basis: Basis::MeasuredDisk,
        growth: Spread::flat(d.growth),
        why: format!(
            "the growth is the median size on disk of {} closed log days: {why}",
            d.days
        ),
        ..base.clone()
    }
}

fn not_counted(input: &Input) -> String {
    match input.counted_since {
        Some(c) => format!(
            "Davy has counted raw log bytes only since {}, too few whole days to measure how they pack",
            time::day_of(c)
        ),
        None => "this server has not counted raw log bytes yet".to_owned(),
    }
}

const SMALL: &str = "no whole closed day could measure how logs pack, as each had under 10 MB of raw logs, a project that retention cut, or no size on disk";

/// All `possible` days measured. A quiet day newer than the oldest of
/// them means the server went quiet since, and the median of their sizes
/// would keep its busy rate: then raw bytes per day, which see the quiet
/// days, times their ratio.
fn measured(base: &Logs, sizes: &[u64], ratios: &[u64], prior: Band, quiet: bool) -> Logs {
    let n = base.own_days;
    let ratio = blend(ratios, base.possible_days, prior);
    let (growth, why) = if quiet {
        (
            raw_times(base.raw, ratio),
            format!(
                "{n} closed log days measured on this server, and a newer day had under 10 MB of raw logs: the growth is raw bytes per day times how those days packed"
            ),
        )
    } else {
        (
            spread(sizes).unwrap_or(Spread::flat(0)),
            format!(
                "{n} closed log days measured on this server: the growth is their median size on disk"
            ),
        )
    };
    Logs {
        basis: Basis::Measured,
        growth,
        ratio: Some(ratio),
        why,
        ..base.clone()
    }
}

/// Raw bytes per day times `ratio`.
fn raw_times(raw: Option<Spread>, ratio: Band) -> Spread {
    let raw = raw.unwrap_or(Spread::flat(0));
    Spread {
        low: times(raw.low, ratio.low, false),
        mid: times(raw.mid, ratio.mid, false),
        high: times(raw.high, ratio.high, true),
    }
}

fn blended(base: &Logs, ratios: &[u64], prior: Band) -> Logs {
    let ratio = blend(ratios, base.possible_days, prior);
    Logs {
        basis: Basis::Blended,
        growth: raw_times(base.raw, ratio),
        ratio: Some(ratio),
        why: format!(
            "{} of {} closed log days measured on this server in the last 30; the rest of the ratio comes from the footprint benchmark",
            base.own_days, base.possible_days
        ),
        ..base.clone()
    }
}

/// Raw bytes per day from all counted bytes over the time counted. One
/// partial day says nothing of the daily cycle, so the range is a factor
/// of 3 under a day counted and 1.5 from then on.
fn first_day(base: &Logs, input: &Input, counted: i64, bytes: u128) -> Logs {
    let per_day = |num: u128, den: u128| {
        clamp(
            bytes * u128::from(DAY_NS.unsigned_abs()) * num
                / den
                / u128::from(counted.unsigned_abs()),
        )
    };
    let (num, den) = if counted < DAY_NS { (3, 1) } else { (3, 2) };
    let raw = Spread {
        low: per_day(den, num),
        mid: per_day(1, 1),
        high: per_day(num, den),
    };
    let p = input.prior;
    let index = p.day_index;
    Logs {
        basis: Basis::FirstDay,
        growth: Spread {
            low: times(raw.low, p.ratio.low, false).saturating_add(index),
            mid: times(raw.mid, p.ratio.mid, false).saturating_add(index),
            high: times(raw.high, p.ratio.high, true).saturating_add(index),
        },
        raw: Some(raw),
        counted_ns: Some(counted),
        ratio: Some(p.ratio),
        why: format!(
            "{:.1} hours of logs counted on this server and no whole day yet: the growth is that rate times the footprint benchmark, so its range is wide",
            hours(counted)
        ),
        ..base.clone()
    }
}

/// `ns` in hours, for the answer.
pub fn hours(ns: i64) -> f64 {
    f64::from(u32::try_from(ns / 1_000_000_000).unwrap_or(u32::MAX)) / 3600.0
}

/// The ratio of the `n` own days weighed against the prior for the rest
/// of `possible`, floored, and ceiled at the high end.
fn blend(ratios: &[u64], possible: usize, prior: Band) -> Band {
    let n = u128::try_from(ratios.len().min(possible)).unwrap_or(0);
    let p = u128::try_from(possible).unwrap_or(1).max(1);
    let rest = p - n;
    let own = spread(ratios).unwrap_or(Spread::flat(0));
    let mix = |own: u64, prior: u64| u128::from(own) * n + u128::from(prior) * rest;
    Band {
        low: clamp(mix(own.low, prior.low) / p),
        mid: clamp(mix(own.mid, prior.mid) / p),
        high: clamp(mix(own.high, prior.high).div_ceil(p)),
    }
}

/// Raw bytes per day of the newest whole days: the middle is the larger
/// of their median and their mean, so a server busy on 3 days of 7, or
/// back from days without logs, does not read as quiet, and the high end
/// is at least the middle.
pub(super) fn raw_per_day(raws: &[u64]) -> Option<Spread> {
    let sum = raws.iter().fold(0_u128, |sum, r| sum + u128::from(*r));
    let mean = clamp(sum / u128::try_from(raws.len()).ok()?.max(1));
    spread(raws).map(|s| {
        let mid = s.mid.max(mean);
        Spread {
            low: s.low,
            mid,
            high: s.high.max(mid),
        }
    })
}

/// The median, and the range of the window: with 5 or more values the
/// second lowest and second highest, so one odd day at each end does not
/// set it; with fewer the minimum and the maximum.
pub(super) fn spread(values: &[u64]) -> Option<Spread> {
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let skip = usize::from(sorted.len() >= 5);
    Some(Spread {
        low: *sorted.get(skip)?,
        mid: median(&sorted)?,
        high: *sorted.get(sorted.len() - 1 - skip)?,
    })
}

/// `bytes` times `ppm` parts per million, rounded up when `up`.
fn times(bytes: u64, ppm: u64, up: bool) -> u64 {
    let product = u128::from(bytes) * u128::from(ppm);
    clamp(if up {
        product.div_ceil(PPM)
    } else {
        product / PPM
    })
}

fn clamp(n: u128) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

#[cfg(test)]
#[path = "growth_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "growth_basis_tests.rs"]
mod basis_tests;

#[cfg(test)]
#[path = "growth_quiet_tests.rs"]
mod quiet_tests;
