use super::tests::{GB, MB, cut, day, days, logs_of, server};
use super::*;
use crate::usage::forecast::{WhatIf, forecast};

/// `busy` days of 10 GB raw and 7 GB on disk, then `quiet_days` of 1 MB
/// up to yesterday, or the other way round with `busy_last`.
fn went_quiet(busy: usize, quiet_days: usize, busy_last: bool) -> Input {
    let loud = (10 * GB, Some(7 * GB));
    let quiet = (MB, Some(MB));
    let (first, second) = if busy_last {
        (quiet, loud)
    } else {
        (loud, quiet)
    };
    let (n_first, n_second) = if busy_last {
        (quiet_days, busy)
    } else {
        (busy, quiet_days)
    };
    let mut values = vec![first; n_first];
    values.extend(std::iter::repeat_n(second, n_second));
    let first_day = day("2026-11-24")
        .checked_sub(i64::try_from(values.len()).unwrap().days())
        .unwrap();
    server(
        "2026-10-01T00:00:00Z",
        "2026-11-24T12:00:00Z",
        &days(&first_day.to_string(), &values),
    )
}

#[test]
fn a_server_that_went_quiet_grows_at_its_quiet_rate() {
    let raw_times_ratio = "7 closed log days measured on this server, and a newer day had under 10 MB of raw logs: the growth is raw bytes per day times how those days packed";
    let median =
        "7 closed log days measured on this server: the growth is their median size on disk";
    let six = "6 of 7 closed log days measured on this server in the last 30; the rest of the ratio comes from the footprint benchmark";
    for (busy, busy_last, basis, growth, days_left, confidence, why) in [
        // The newest 7 whole days are all quiet: their median and their
        // mean are 1 MB.
        (
            7,
            false,
            Basis::Measured,
            Spread::flat(700_000),
            None,
            Confidence::High,
            raw_times_ratio,
        ),
        (
            6,
            false,
            Basis::Blended,
            Spread {
                low: 680_514,
                mid: 708_714,
                high: 743_443,
            },
            None,
            Confidence::Medium,
            six,
        ),
        // Quiet days older than the busy ones do not lower the growth.
        (
            7,
            true,
            Basis::Measured,
            Spread::flat(7 * GB),
            Some(2),
            Confidence::High,
            median,
        ),
    ] {
        let f = forecast(&went_quiet(busy, 19, busy_last), &WhatIf::default()).unwrap();
        let l = f.logs.clone().unwrap();
        assert_eq!(
            (
                l.basis,
                l.own_days,
                l.growth,
                f.days_left,
                l.confidence(false),
                l.why.as_str()
            ),
            (basis, busy, growth, days_left, confidence, why),
            "{busy} {busy_last}"
        );
    }
}

/// The growth, the days left and the confidence of `input`.
fn answer(input: &Input) -> (Basis, Spread, Option<u64>, Confidence) {
    let f = forecast(input, &WhatIf::default()).unwrap();
    let l = f.logs.unwrap();
    (l.basis, l.growth, f.days_left, l.confidence(false))
}

#[test]
fn a_server_busy_on_monday_wednesday_and_friday_is_not_read_as_quiet() {
    // 2026-11-24 is a Tuesday. Busy days hold 10 GB raw and 7 GB on disk,
    // the others 1 MB. The disk takes 3 GB a day.
    let from = day("2026-10-25");
    let all: Vec<(Date, u64, Option<u64>)> = (0..30)
        .map(|i| {
            let d = from.checked_add(i.days()).unwrap();
            if [1, 3, 5].contains(&d.weekday().to_monday_one_offset()) {
                (d, 10 * GB, Some(7 * GB))
            } else {
                (d, MB, Some(MB))
            }
        })
        .collect();
    let input = server("2026-10-01T00:00:00Z", "2026-11-24T12:00:00Z", &all);
    // 30 GB and 4 MB in the newest 7 whole days, times 700,000 ppm.
    assert_eq!(
        answer(&input),
        (
            Basis::Measured,
            Spread {
                low: 700_000,
                mid: 3_000_399_999,
                high: 7 * GB,
            },
            Some(4),
            Confidence::High
        )
    );
}

#[test]
fn a_server_that_stopped_4_days_and_restarted_is_not_read_as_quiet() {
    // 10 busy days, 4 days without any logs, then 1 to 3 busy days: the
    // newest 7 whole days always hold 3 busy ones.
    for back in 1..=3 {
        let mut values = vec![(10 * GB, Some(7 * GB)); 10];
        values.extend([(0, None); 4]);
        values.extend(std::iter::repeat_n((10 * GB, Some(7 * GB)), back));
        let first = day("2026-11-24")
            .checked_sub(i64::try_from(values.len()).unwrap().days())
            .unwrap();
        let input = server(
            "2026-10-01T00:00:00Z",
            "2026-11-24T12:00:00Z",
            &days(&first.to_string(), &values),
        );
        assert_eq!(
            answer(&input),
            (
                Basis::Measured,
                Spread {
                    low: 0,
                    mid: 2_999_999_999,
                    high: 7 * GB,
                },
                Some(4),
                Confidence::High
            ),
            "{back}"
        );
    }
}

#[test]
fn a_server_that_went_quiet_falls_to_its_quiet_rate_over_7_days() {
    // 20 busy days, then `quiet` closed days of 1 MB.
    for (quiet, growth, days_left) in [
        (
            3,
            Spread {
                low: 700_000,
                mid: 7 * GB,
                high: 7 * GB,
            },
            Some(2),
        ),
        (
            4,
            Spread {
                low: 700_000,
                mid: 3_000_399_999,
                high: 7 * GB,
            },
            Some(4),
        ),
        (
            6,
            Spread {
                low: 700_000,
                mid: 1_000_599_999,
                high: 1_000_600_000,
            },
            Some(10),
        ),
        (7, Spread::flat(700_000), None),
    ] {
        let (basis, g, left, _) = answer(&went_quiet(20, quiet, false));
        assert_eq!(
            (basis, g, left),
            (Basis::Measured, growth, days_left),
            "{quiet}"
        );
    }
}

#[test]
fn a_day_of_exactly_10_mb_is_not_quiet() {
    let raw_times_ratio = "7 closed log days measured on this server, and a newer day had under 10 MB of raw logs: the growth is raw bytes per day times how those days packed";
    let median =
        "7 closed log days measured on this server: the growth is their median size on disk";
    for (raw, why) in [(MIN_RAW - 1, raw_times_ratio), (MIN_RAW, median)] {
        // 7 busy days, then yesterday without a size on disk.
        let mut values = vec![(10 * GB, Some(7 * GB)); 7];
        values.push((raw, None));
        let input = server(
            "2026-10-01T00:00:00Z",
            "2026-11-24T12:00:00Z",
            &days("2026-11-16", &values),
        );
        let l = logs_of(&input).unwrap();
        assert_eq!(
            (l.basis, l.own_days, l.growth, l.why.as_str()),
            (Basis::Measured, 7, Spread::flat(7 * GB), why),
            "{raw}"
        );
    }
}

#[test]
fn the_30th_closed_day_back_still_counts_for_the_ratio() {
    for (quiet, basis, own, high) in [
        (23, Basis::Measured, 7, 700_000),
        (24, Basis::Blended, 6, 743_443),
    ] {
        let l = logs_of(&went_quiet(7, quiet, false)).unwrap();
        assert_eq!(
            (l.basis, l.own_days, l.growth.high),
            (basis, own, high),
            "{quiet}"
        );
    }
}

#[test]
fn raw_bytes_per_day_read_7_days_under_a_shorter_retention() {
    // The server keeps logs 3 days. Retention cut the 4 older days, whose
    // raw bytes `log_days` still holds.
    let mut values = vec![(20 * GB, None); 4];
    values.extend([(10 * GB, Some(7 * GB)); 3]);
    let mut input = server(
        "2026-10-01T00:00:00Z",
        "2026-11-24T12:00:00Z",
        &days("2026-11-17", &values),
    );
    input.projects[0].1 = 3;
    for d in ["2026-11-17", "2026-11-18", "2026-11-19", "2026-11-20"] {
        cut(&mut input, d);
    }
    let l = logs_of(&input).unwrap();
    assert_eq!(
        (l.basis, l.own_days, l.growth, l.raw, l.raw_days),
        (
            Basis::Measured,
            3,
            Spread::flat(7 * GB),
            Some(Spread {
                low: 10 * GB,
                mid: 20 * GB,
                high: 20 * GB,
            }),
            7
        )
    );
}
