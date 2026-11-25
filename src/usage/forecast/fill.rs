//! When used space reaches its limit, for parts of Davy that each grow by
//! a number of bytes a day until they have taken their room: the logs of
//! a project, or the error events of one.

use jiff::{Span, civil::Date};

/// Days past which the answer reads "beyond 10 years": a forecast that far
/// out says more about the rate staying the same than about the disk.
pub const HORIZON_DAYS: u64 = 3_650;

/// One part of what Davy grows: bytes a day, until it has added `room`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Part {
    pub growth: u64,
    pub room: u64,
}

/// The least whole number of days `t` with `used` plus the sum over the
/// parts of `min(growth × t, room)` at or past `limit`, or `None` when
/// that never happens. A disk already past the limit gives 0 while a part
/// still grows, and `None` once none does: other files filled it.
pub fn days_left(used: u64, limit: u64, parts: &[Part]) -> Option<u64> {
    let mut parts: Vec<Part> = parts
        .iter()
        .copied()
        .filter(|p| p.growth > 0 && p.room > 0)
        .collect();
    if parts.is_empty() {
        return None;
    }
    if used >= limit {
        return Some(0);
    }
    let need = u128::from(limit - used);
    // A part stops growing at its knee, room / growth days: sort by it.
    parts.sort_by(|a, b| {
        let a_knee = u128::from(a.room) * u128::from(b.growth);
        let b_knee = u128::from(b.room) * u128::from(a.growth);
        a_knee.cmp(&b_knee)
    });
    let mut full: u128 = 0;
    let mut rate: u128 = parts.iter().map(|p| u128::from(p.growth)).sum();
    for p in &parts {
        // At this part's knee, used space is `full` plus `rate` times the
        // knee. Past `need` there, the day falls before the knee.
        let left = need - full;
        if left.saturating_mul(u128::from(p.growth)) <= rate.saturating_mul(u128::from(p.room)) {
            return Some(u64::try_from(left.div_ceil(rate)).unwrap_or(u64::MAX));
        }
        full += u128::from(p.room);
        rate -= u128::from(p.growth);
    }
    // Every part took its room short of the limit.
    None
}

/// Days left, the day the disk fills and whether that is beyond
/// [`HORIZON_DAYS`], which leaves both `None`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fill {
    pub days_left: Option<u64>,
    pub full_on: Option<Date>,
    pub beyond_10_years: bool,
}

impl Fill {
    /// `days` from `today`, `None` for never.
    pub fn new(today: Date, days: Option<u64>) -> Self {
        match days {
            Some(n) if n > HORIZON_DAYS => Self {
                days_left: None,
                full_on: None,
                beyond_10_years: true,
            },
            _ => Self {
                days_left: days,
                full_on: days.map(|n| {
                    let span = i64::try_from(n)
                        .ok()
                        .and_then(|n| Span::new().try_days(n).ok());
                    span.and_then(|s| today.checked_add(s).ok())
                        .unwrap_or(Date::MAX)
                }),
                beyond_10_years: false,
            },
        }
    }
}

#[cfg(test)]
#[path = "fill_tests.rs"]
mod tests;
