//! Clock reads, timestamp parsing and day arithmetic. Logs keep unix nanoseconds
//! and error events unix milliseconds, both as `i64`. Days are UTC.

use jiff::{Timestamp, civil, tz::TimeZone};
use std::time::Duration;

pub fn now_ms() -> i64 {
    Timestamp::now().as_millisecond()
}

pub fn now_ns() -> i64 {
    nanos(Timestamp::now()).unwrap_or(i64::MAX)
}

/// An RFC 3339 timestamp, or a date and time without an offset, read as UTC.
pub fn parse(s: &str) -> Option<Timestamp> {
    s.parse::<Timestamp>().ok().or_else(|| {
        let local: civil::DateTime = s.parse().ok()?;
        TimeZone::UTC.to_timestamp(local).ok()
    })
}

/// `None` outside the years 1677 to 2262, which `i64` nanoseconds cannot hold.
pub fn nanos(t: Timestamp) -> Option<i64> {
    i64::try_from(t.as_nanosecond()).ok()
}

fn from_nanos(ns: i64) -> Timestamp {
    Timestamp::from_nanosecond(i128::from(ns)).expect("every i64 nanosecond is a valid timestamp")
}

pub fn day_of(ns: i64) -> civil::Date {
    TimeZone::UTC.to_datetime(from_nanos(ns)).date()
}

/// RFC 3339 in UTC, with as many fraction digits as the value needs.
pub fn rfc3339(ns: i64) -> String {
    from_nanos(ns).to_string()
}

/// `rfc3339` for unix milliseconds, the unit of error events.
pub fn rfc3339_ms(ms: i64) -> String {
    rfc3339(ms.saturating_mul(1_000_000))
}

/// A span such as `30s`, `15m`, `1h` or `2d`, or `None` when `s` is not one
/// or does not fit.
pub fn duration(s: &str) -> Option<Duration> {
    let (count, unit_secs) = [('s', 1), ('m', 60), ('h', 3600), ('d', 86_400)]
        .into_iter()
        .find_map(|(suffix, secs)| Some((s.strip_suffix(suffix)?, secs)))?;
    let count: u64 = count.parse().ok()?;
    count.checked_mul(unit_secs).map(Duration::from_secs)
}

/// A point in time from a CLI flag or a tool argument: an RFC 3339 timestamp,
/// or a duration back from `now_ns` such as `30s`, `15m`, `1h` or `2d`.
pub fn instant(s: &str, now_ns: i64) -> Result<i64, String> {
    if let Some(t) = parse(s) {
        return nanos(t).ok_or_else(|| format!("{s:?} is out of range"));
    }
    duration(s)
        .and_then(|back| i64::try_from(back.as_nanos()).ok())
        .and_then(|back| now_ns.checked_sub(back))
        .ok_or_else(|| format!("expected 30m, 1h, 2d or an RFC 3339 timestamp, got {s:?}"))
}

/// The integer part of `n`, or `None` when `n` is not finite or does not fit an `i64`.
#[expect(
    clippy::cast_possible_truncation,
    reason = "the range is checked first"
)]
pub fn whole(n: f64) -> Option<i64> {
    const TWO_POW_63: f64 = 9_223_372_036_854_775_808.0;
    (n.is_finite() && (-TWO_POW_63..TWO_POW_63).contains(&n)).then_some(n as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instant_reads_a_timestamp_or_a_duration_back_from_now() {
        let now = 1_795_164_121_054_000_000;
        assert_eq!(instant("90s", now), Ok(now - 90_000_000_000));
        assert_eq!(instant("2d", now), Ok(now - 172_800_000_000_000));
        assert_eq!(instant("2026-11-20", now), Ok(1_795_132_800_000_000_000));
        for bad in ["", "h", "1w", "-", "99999999999d"] {
            assert!(instant(bad, now).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn duration_reads_a_count_and_a_unit() {
        assert_eq!(duration("90s"), Some(Duration::from_secs(90)));
        assert_eq!(duration("5m"), Some(Duration::from_mins(5)));
        assert_eq!(duration("2d"), Some(Duration::from_hours(48)));
        assert_eq!(duration("0s"), Some(Duration::ZERO));
        for bad in ["", "s", "1w", "-1s", "1.5h", "99999999999999999999d"] {
            assert_eq!(duration(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn parse_reads_offsets_and_treats_a_missing_zone_as_utc() {
        let ns = |s| parse(s).and_then(nanos);
        assert_eq!(
            ns("2026-11-20T08:42:01.054Z"),
            Some(1_795_164_121_054_000_000)
        );
        assert_eq!(
            ns("2026-11-20T09:42:01.054+01:00"),
            Some(1_795_164_121_054_000_000)
        );
        assert_eq!(
            ns("2026-11-20T08:42:01.054"),
            Some(1_795_164_121_054_000_000)
        );
        assert_eq!(ns("2026-11-20"), Some(1_795_132_800_000_000_000));
        assert_eq!(ns("yesterday"), None);
        assert_eq!(ns("1500-01-01T00:00:00Z"), None);
    }

    #[test]
    fn rfc3339_prints_only_the_fraction_it_needs() {
        assert_eq!(rfc3339(1_795_164_121_000_000_000), "2026-11-20T08:42:01Z");
        assert_eq!(
            rfc3339(1_795_164_121_054_000_000),
            "2026-11-20T08:42:01.054Z"
        );
        assert_eq!(
            rfc3339(1_795_164_121_054_000_001),
            "2026-11-20T08:42:01.054000001Z"
        );
    }

    #[test]
    fn whole_rejects_what_i64_cannot_hold() {
        assert_eq!(whole(1.9), Some(1));
        assert_eq!(whole(-1.9), Some(-1));
        assert_eq!(whole(f64::NAN), None);
        assert_eq!(whole(9.3e18), None);
    }
}
