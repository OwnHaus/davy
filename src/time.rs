//! Clock reads, timestamp parsing and day arithmetic. Logs keep unix nanoseconds
//! and error events unix milliseconds, both as `i64`. Days are UTC.

use jiff::{Timestamp, civil, tz::TimeZone};

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
