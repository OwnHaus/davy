//! When a heartbeat expects its next ping: after a fixed interval, after
//! calendar months in a time zone, or at the next time a crontab names.
//!
//! Everything here is pure. Times go in and out as Unix milliseconds, the
//! unit the store keeps.

use jiff::{
    Span, Timestamp,
    civil::{Date, DateTime},
    tz::TimeZone,
};
use std::time::Duration;

const MAX_ZONE: usize = 64;
const MAX_CRONTAB: usize = 200;
/// A crontab must match within this many years, so `0 0 29 2 *` passes and
/// the search for the next match always ends.
const SEARCH_YEARS: i16 = 8;
pub const EVERY: (Duration, Duration) = (Duration::from_mins(1), Duration::from_hours(366 * 24));
pub const MAX_MONTHS: u32 = 12;

/// A time zone from the bundled IANA database, so a name means the same rules
/// on every host, a container without zoneinfo included.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Zone(TimeZone);

impl Zone {
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        if name.is_empty() || name.len() > MAX_ZONE {
            return None;
        }
        let tz = TimeZone::get(name).ok()?;
        tz.iana_name().is_some().then_some(Self(tz))
    }

    #[must_use]
    pub fn utc() -> Self {
        Self(TimeZone::UTC)
    }

    /// The canonical spelling, `Europe/Prague` for `europe/prague`.
    #[must_use]
    pub fn name(&self) -> &str {
        self.0.iana_name().unwrap_or("UTC")
    }
}

/// A 5-field crontab: minute, hour, day of month, month, day of week. Each
/// field is a set of the values it matches, one bit per value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Crontab {
    text: String,
    minutes: u64,
    hours: u64,
    days: u64,
    months: u64,
    weekdays: u64,
    /// Vixie cron matches a day by either day field when both are restricted,
    /// and a field that starts with `*` counts as unrestricted.
    days_star: bool,
    weekdays_star: bool,
}

const MONTHS: [&str; 12] = [
    "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
];
const WEEKDAYS: [&str; 7] = ["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"];

impl Crontab {
    /// Refuses an expression that can never match, such as `0 0 31 2 *`.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        if s.len() > MAX_CRONTAB {
            return None;
        }
        let expanded = match s.trim().to_ascii_lowercase().as_str() {
            "@yearly" | "@annually" => "0 0 1 1 *",
            "@monthly" => "0 0 1 * *",
            "@weekly" => "0 0 * * 0",
            "@daily" | "@midnight" => "0 0 * * *",
            "@hourly" => "0 * * * *",
            _ => s,
        };
        let fields: Vec<&str> = expanded.split_ascii_whitespace().collect();
        let [minute, hour, day, month, weekday] = fields[..] else {
            return None;
        };
        let mut weekdays = field(weekday, 0, 7, &WEEKDAYS)?;
        if weekdays & 1 << 7 != 0 {
            weekdays = (weekdays & !(1 << 7)) | 1;
        }
        let tab = Self {
            text: fields.join(" "),
            minutes: field(minute, 0, 59, &[])?,
            hours: field(hour, 0, 23, &[])?,
            days: field(day, 1, 31, &[])?,
            months: field(month, 1, 12, &MONTHS)?,
            weekdays,
            days_star: day.starts_with('*'),
            weekdays_star: weekday.starts_with('*'),
        };
        tab.can_match().then_some(tab)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// A restricted weekday field matches some day of every month, so only a
    /// day of month alone can be impossible, like the 30th of February.
    fn can_match(&self) -> bool {
        if self.days_star || !self.weekdays_star {
            return true;
        }
        (1..=12).any(|m| self.months & 1 << m != 0 && self.days & mask(1, longest(m)) != 0)
    }

    fn day_matches(&self, date: Date) -> bool {
        let day = self.days & 1 << date.day() != 0;
        let weekday = self.weekdays & 1 << date.weekday().to_sunday_zero_offset() != 0;
        match (self.days_star, self.weekdays_star) {
            (false, false) => day || weekday,
            _ => day && weekday,
        }
    }

    /// The first matching minute after `after`, in `zone`. A time that a DST
    /// change skips runs at the offset before the change, and a time that
    /// happens twice runs at the first.
    #[must_use]
    pub fn next(&self, zone: &Zone, after: Timestamp) -> Option<Timestamp> {
        let start = zone.0.to_datetime(after);
        let mut date = start.date();
        let (mut hour, mut minute) = (start.hour(), start.minute() + 1);
        let end = date.year().checked_add(SEARCH_YEARS)?;
        while date.year() <= end {
            if minute > 59 {
                (hour, minute) = (hour + 1, 0);
            }
            if hour > 23 {
                (date, hour, minute) = (date.tomorrow().ok()?, 0, 0);
            }
            if self.months & 1 << date.month() == 0 {
                (date, hour, minute) = (date.last_of_month().tomorrow().ok()?, 0, 0);
            } else if !self.day_matches(date) {
                (date, hour, minute) = (date.tomorrow().ok()?, 0, 0);
            } else if self.hours & 1 << hour == 0 {
                (hour, minute) = (hour + 1, 0);
            } else if self.minutes & 1 << minute == 0 {
                minute += 1;
            } else {
                let at = DateTime::new(date.year(), date.month(), date.day(), hour, minute, 0, 0);
                let at = zone.0.to_ambiguous_timestamp(at.ok()?).compatible().ok()?;
                if at > after {
                    return Some(at);
                }
                minute += 1;
            }
        }
        None
    }
}

/// The days of month `m` in a leap year.
fn longest(m: i8) -> i8 {
    match m {
        2 => 29,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

fn mask(low: i8, high: i8) -> u64 {
    (low..=high).fold(0, |m, v| m | 1 << v)
}

/// One crontab field as the set of values it matches, `low..=high`. `names`
/// spell the values from `low` in order, case-insensitive.
fn field(text: &str, low: i8, high: i8, names: &[&str]) -> Option<u64> {
    let value = |s: &str| -> Option<i8> {
        if let Some(i) = names.iter().position(|n| n.eq_ignore_ascii_case(s)) {
            return i8::try_from(i).ok()?.checked_add(low);
        }
        let v: i8 = s.parse().ok()?;
        (low..=high).contains(&v).then_some(v)
    };
    let mut set = 0;
    for part in text.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((range, step)) => (range, Some(step.parse::<i8>().ok()?)),
            None => (part, None),
        };
        let (from, to) = match (range, range.split_once('-')) {
            ("*", _) => (low, high),
            (_, Some((a, b))) => (value(a)?, value(b)?),
            (a, None) if step.is_some() => (value(a)?, high),
            (a, None) => (value(a)?, value(a)?),
        };
        let step = step.unwrap_or(1);
        if from > to || !(1..=high - low + 1).contains(&step) {
            return None;
        }
        let mut v = from;
        while v <= to {
            set |= 1 << v;
            v = v.checked_add(step)?;
        }
    }
    Some(set)
}

/// What a heartbeat expects. Only these three shapes can be stored, and a
/// `CHECK` in the schema holds the columns to them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Schedule {
    /// The minute, hour, day and week intervals of the SDKs, and every
    /// heartbeat made by `davy monitor add`.
    Every(Duration),
    /// Month and year intervals, counted in calendar months in the zone.
    Months {
        n: u32,
        zone: Zone,
    },
    Cron {
        spec: Crontab,
        zone: Zone,
    },
}

impl Schedule {
    /// An SDK interval: `value` times `unit`, from 1 minute to 1 year.
    #[must_use]
    pub fn interval(value: u64, unit: &str, zone: Zone) -> Option<Self> {
        let secs = match unit {
            "minute" => 60,
            "hour" => 3600,
            "day" => 86_400,
            "week" => 604_800,
            "month" | "year" => {
                let n = value.checked_mul(if unit == "year" { 12 } else { 1 })?;
                let n = u32::try_from(n)
                    .ok()
                    .filter(|n| (1..=MAX_MONTHS).contains(n))?;
                return Some(Self::Months { n, zone });
            }
            _ => return None,
        };
        let every = Duration::from_secs(value.checked_mul(secs)?);
        (EVERY.0..=EVERY.1)
            .contains(&every)
            .then_some(Self::Every(every))
    }

    /// When the next ping is due after one at `at`, at most `margin` late.
    /// For a crontab, a ping up to `margin` before a scheduled time counts as
    /// that time's, so the next expected one is the time after it.
    #[must_use]
    pub fn due(&self, at: i64, margin: Duration) -> Option<i64> {
        let margin = i64::try_from(margin.as_millis()).ok()?;
        let next = match self {
            Self::Every(every) => at.checked_add(i64::try_from(every.as_millis()).ok()?)?,
            Self::Months { n, zone } => {
                let start = Timestamp::from_millisecond(at)
                    .ok()?
                    .to_zoned_datetime(zone.0.clone());
                start
                    .checked_add(Span::new().months(*n))
                    .ok()?
                    .timestamp()
                    .as_millisecond()
            }
            Self::Cron { spec, zone } => {
                let after = Timestamp::from_millisecond(at.checked_add(margin)?).ok()?;
                spec.next(zone, after)?.as_millisecond()
            }
        };
        next.checked_add(margin)
    }

    /// The schedule from the columns `every_s`, `months`, `cron` and `zone`.
    #[must_use]
    pub fn from_columns(
        every_s: i64,
        months: Option<i64>,
        cron: Option<&str>,
        zone: Option<&str>,
    ) -> Option<Self> {
        let zone = zone.map(Zone::parse);
        match (months, cron, zone) {
            (None, None, None) => Some(Self::Every(Duration::from_secs(
                u64::try_from(every_s).ok()?,
            ))),
            (Some(n), None, Some(zone)) => Some(Self::Months {
                n: u32::try_from(n).ok()?,
                zone: zone?,
            }),
            (None, Some(c), Some(zone)) => Some(Self::Cron {
                spec: Crontab::parse(c)?,
                zone: zone?,
            }),
            _ => None,
        }
    }

    /// The columns `every_s`, `months`, `cron` and `zone` for this schedule.
    #[must_use]
    pub fn columns(&self) -> (i64, Option<i64>, Option<&str>, Option<&str>) {
        match self {
            Self::Every(every) => (
                i64::try_from(every.as_secs()).unwrap_or(i64::MAX),
                None,
                None,
                None,
            ),
            Self::Months { n, zone } => (0, Some(i64::from(*n)), None, Some(zone.name())),
            Self::Cron { spec, zone } => (0, None, Some(spec.as_str()), Some(zone.name())),
        }
    }
}

impl std::fmt::Display for Schedule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Every(every) => write!(f, "every {}", span(*every)),
            Self::Months { n: 1, zone } => write!(f, "every month, {}", zone.name()),
            Self::Months { n, zone } => write!(f, "every {n} months, {}", zone.name()),
            Self::Cron { spec, zone } => write!(f, "cron {}, {}", spec.as_str(), zone.name()),
        }
    }
}

/// `2h`, `90m`, `1w`: the largest unit that divides the duration evenly.
#[must_use]
pub fn span(d: Duration) -> String {
    let s = d.as_secs();
    let units = [(604_800, "w"), (86_400, "d"), (3600, "h"), (60, "m")];
    match units.iter().find(|(u, _)| s > 0 && s.is_multiple_of(*u)) {
        Some((u, name)) => format!("{}{name}", s / u),
        None => format!("{s}s"),
    }
}
