use super::*;
use crate::usage::forecast::Tiers;
use crate::usage::{
    footprint::FIXTURE,
    forecast::{ErrorBasis, Errors, Estimate, Fill, Lean, Logs, Spread},
};
use jiff::civil::Date;

const HOUR: i64 = 3_600_000_000_000;

/// A forecast on `basis` that puts the disk full in `days`, with both
/// projects keeping logs 30 days.
fn full_in(basis: Basis, days: u64) -> Forecast {
    let today: Date = "2026-11-24".parse().unwrap();
    let n = i64::try_from(days).unwrap();
    let full_on = Some(today.checked_add(jiff::Span::new().days(n)).unwrap());
    Forecast {
        growth: GIB,
        longest_days: 30,
        shortest_days: 30,
        steady_state: 100 * GIB,
        tiers: Tiers::ALL_HOT,
        headroom: 10 * GIB,
        days_left: Some(days),
        full_on,
        errors: None,
        beyond_10_years: false,
        estimate: None,
        unseen: None,
        logs: Some(Logs {
            basis,
            growth: Spread::flat(GIB),
            own_days: 0,
            possible_days: 7,
            raw: None,
            raw_days: 0,
            counted_ns: (basis == Basis::FirstDay).then_some(3 * HOUR / 2),
            recent: true,
            ratio: None,
            own_ratio: None,
            prior: FIXTURE,
            why: String::new(),
        }),
        full_on_range: [full_on; 2],
    }
}

#[test]
fn a_first_day_forecast_only_warns_while_the_disk_has_days_left() {
    let data = Path::new("/var/lib/davy");
    let free = Some(40 * GIB);
    let first = "the day rests on 1.5 hours of logs and the footprint benchmark; run doctor again once a whole day has closed before you shorten a retention with davy project retention SLUG --data /var/lib/davy --logs N, or add disk";
    let measured = "shorten retention with davy project retention SLUG --data /var/lib/davy --logs N, or add disk";
    for (basis, days, status, detail, fix) in [
        (
            Basis::FirstDay,
            1,
            Status::Warn,
            "40 GiB free, forecast full on 2026-11-25, in 1 day, a first day forecast from 1.5 hours of logs, low confidence",
            first,
        ),
        (
            Basis::FirstDay,
            0,
            Status::Fail,
            "40 GiB free, forecast full on 2026-11-24, in 0 days",
            first,
        ),
        (
            Basis::Measured,
            1,
            Status::Fail,
            "40 GiB free, forecast full on 2026-11-25, in 1 day",
            measured,
        ),
        (
            Basis::Blended,
            1,
            Status::Fail,
            "40 GiB free, forecast full on 2026-11-25, in 1 day",
            measured,
        ),
    ] {
        assert_eq!(
            disk(data, free, Some(Ok(full_in(basis, days)))),
            Check {
                name: "disk",
                status,
                detail: detail.to_owned(),
                fix: Some(fix.to_owned()),
            },
            "{basis:?} {days}"
        );
    }
}

fn with_estimate(mut f: Forecast, days: Option<u64>) -> Forecast {
    let today: Date = "2026-11-24".parse().unwrap();
    f.estimate = Some(Estimate {
        steady_state: 80 * GIB,
        fill: Fill::new(today, days),
        clamped: false,
    });
    f
}

/// A worst case: api keeps logs 30 days, shop 7.
fn worst(days: u64) -> Forecast {
    Forecast {
        shortest_days: 7,
        ..full_in(Basis::Measured, days)
    }
}

#[test]
fn the_estimate_shows_its_day_when_it_differs_from_the_bound() {
    let data = Path::new("/var/lib/davy");
    let free = Some(40 * GIB);
    let worst_fix = "the day takes the logs of every project as kept 30 days; watch free space over a few days before you shorten a retention with davy project retention SLUG --data /var/lib/davy --logs N, or add disk";
    let earliest = "40 GiB free, forecast full on 2026-12-04, in 10 days at the earliest at the current rate, if every project kept logs 30 days";
    for (estimate, status, also) in [
        (Some(10), Status::Warn, String::new()),
        (
            Some(13),
            Status::Warn,
            "; with each project's logs kept as long as it keeps them and packing alike, the estimate is full on 2026-12-07, in 13 days".to_owned(),
        ),
        (
            Some(40),
            Status::Warn,
            "; with each project's logs kept as long as it keeps them and packing alike, the estimate is full on 2027-01-03, in 40 days".to_owned(),
        ),
        (
            Some(4_000),
            Status::Warn,
            "; with each project's logs kept as long as it keeps them and packing alike, the estimate is full beyond 10 years".to_owned(),
        ),
        (
            None,
            Status::Warn,
            "; with each project's logs kept as long as it keeps them and packing alike, the estimate is never full".to_owned(),
        ),
    ] {
        assert_eq!(
            disk(data, free, Some(Ok(with_estimate(worst(10), estimate)))),
            Check {
                name: "disk",
                status,
                detail: format!("{earliest}{also}"),
                fix: Some(worst_fix.to_owned()),
            },
            "{estimate:?}"
        );
    }
}

/// The estimate never raises the status: the bound does, as the
/// estimate assumes every project's logs pack alike. Outside a worst case
/// it changes nothing, even under 2 days.
#[test]
fn the_estimate_never_raises_the_status() {
    let data = Path::new("/var/lib/davy");
    let free = Some(40 * GIB);
    for (bound, estimate, status) in [
        (20, Some(20), Status::Ok),
        (20, Some(13), Status::Ok),
        (20, None, Status::Ok),
        (10, Some(10), Status::Warn),
        (10, Some(40), Status::Warn),
        (10, None, Status::Warn),
        (1, Some(40), Status::Fail),
        (1, None, Status::Fail),
    ] {
        let check = disk(
            data,
            free,
            Some(Ok(with_estimate(full_in(Basis::Measured, bound), estimate))),
        );
        assert_eq!(check.status, status, "{bound} {estimate:?}");
    }
}

/// A worst case under 2 days only warns where the estimate puts the disk
/// full later, or never, or there is none: the projects' own retentions
/// may save it. Where the estimate lands on its day, it fails.
#[test]
fn a_worst_case_fails_where_the_estimate_lands_on_its_day() {
    let data = Path::new("/var/lib/davy");
    let free = Some(40 * GIB);
    for (estimate, status) in [
        (Some(Some(1)), Status::Fail),
        (Some(Some(2)), Status::Warn),
        (Some(None), Status::Warn),
        (None, Status::Warn),
    ] {
        let f = match estimate {
            Some(days) => with_estimate(worst(1), days),
            None => worst(1),
        };
        assert_eq!(disk(data, free, Some(Ok(f))).status, status, "{estimate:?}");
    }
    // A bound past the limit fails whatever the estimate says.
    let check = disk(data, free, Some(Ok(with_estimate(worst(0), None))));
    assert_eq!(check.status, Status::Fail);
}

#[test]
fn a_disk_full_beyond_10_years_is_ok() {
    let data = Path::new("/var/lib/davy");
    let beyond = Forecast {
        days_left: None,
        full_on: None,
        beyond_10_years: true,
        full_on_range: [None; 2],
        ..full_in(Basis::Measured, 20)
    };
    let said = "40 GiB free, forecast full beyond 10 years at the current rate";
    for (estimate, also) in [
        (Some(5_000), ""),
        // A bound beyond 10 years and an estimate of never differ: one
        // project, so the estimate differs by the middle growth only.
        (
            None,
            "; at the middle growth of logs, the estimate is never full",
        ),
    ] {
        let check = disk(
            data,
            Some(40 * GIB),
            Some(Ok(with_estimate(beyond.clone(), estimate))),
        );
        assert_eq!(
            (check.status, check.detail),
            (Status::Ok, format!("{said}{also}")),
            "{estimate:?}"
        );
    }
    // Errors do not change the words: the disk fills, beyond 10 years.
    let errors = Forecast {
        errors: Some(Errors {
            projects: 1,
            events_per_day: 10_000,
            bytes_per_event: 2_100,
            own_days: 0,
            basis: ErrorBasis::Footprint,
            growth: 21_000_000,
            estimate_growth: 21_000_000,
            share_ppm: 1_000_000,
        }),
        ..beyond
    };
    let check = disk(data, Some(40 * GIB), Some(Ok(errors)));
    assert_eq!((check.status, check.detail.as_str()), (Status::Ok, said));
}

/// A forecast on errors alone, `days` left or never, with projects keeping
/// logs 7 and 30 days: retentions of logs that add nothing make no worst
/// case.
fn errors_only(days: Option<u64>) -> Forecast {
    let mut f = full_in(
        Basis::ErrorsOnly {
            lean: Lean::Benchmark(0),
            logs_pending: false,
        },
        days.unwrap_or(0),
    );
    f.days_left = days;
    f.full_on = f.full_on.filter(|_| days.is_some());
    f.growth = 0;
    f.shortest_days = 7;
    f.errors = Some(Errors {
        projects: 1,
        events_per_day: 100_000,
        bytes_per_event: 1_600,
        own_days: 0,
        basis: ErrorBasis::Footprint,
        growth: 160_000_000,
        estimate_growth: 160_000_000,
        share_ppm: 1_000_000,
    });
    if let Some(logs) = &mut f.logs {
        logs.growth = Spread::flat(0);
    }
    f
}

#[test]
fn a_forecast_on_errors_alone_warns_and_fails_as_any_other() {
    let data = Path::new("/var/lib/davy");
    let fix = "shorten retention with davy project retention SLUG --data /var/lib/davy --errors N, or add disk";
    for (days, status, detail, fix) in [
        (
            Some(1),
            Status::Fail,
            "40 GiB free, forecast full on 2026-11-25, in 1 day",
            Some(fix),
        ),
        (
            Some(10),
            Status::Warn,
            "40 GiB free, forecast full on 2026-12-04, in 10 days",
            Some(fix),
        ),
        (
            Some(14),
            Status::Ok,
            "40 GiB free, forecast full on 2026-12-08, in 14 days",
            None,
        ),
        (
            None,
            Status::Ok,
            "40 GiB free, errors never fill it at the current rate",
            None,
        ),
    ] {
        let check = disk(data, Some(40 * GIB), Some(Ok(errors_only(days))));
        assert_eq!(
            (check.status, check.detail.as_str(), check.fix.as_deref()),
            (status, detail, fix),
            "{days:?}"
        );
    }
    let beyond = Forecast {
        beyond_10_years: true,
        ..errors_only(None)
    };
    let check = disk(data, Some(40 * GIB), Some(Ok(beyond)));
    assert_eq!(
        (check.status, check.detail.as_str()),
        (
            Status::Ok,
            "40 GiB free, forecast full beyond 10 years at the current rate"
        )
    );
}

/// A server past its limit with 100 000 errors a day from 2026-11-15 and
/// no logs, counting since 2026-11-10. Its first 10 log records, 1 869
/// raw bytes today, have no log basis: the forecast still rests on the
/// errors, so doctor still fails.
#[test]
fn a_first_log_line_does_not_clear_a_failing_errors_forecast() {
    use crate::usage::{
        Count,
        forecast::{Input, ProjectErrors, RawDay, WhatIf, forecast},
    };
    let day = |s: &str| -> Date { s.parse().unwrap() };
    let first = day("2026-11-15");
    let mut input = Input {
        today: day("2026-11-25"),
        closed: Vec::new(),
        davy: 3 * GIB,
        fixed: 3 * GIB,
        used: 460 * GIB,
        total: 500 * GIB,
        projects: vec![("api".to_owned(), 30)],
        default_days: 30,
        cold: Vec::new(),
        hot_days: None,
        raw: Vec::new(),
        counted_since: Some(crate::time::midnight(day("2026-11-10"))),
        now: crate::time::midnight(day("2026-11-25")) + 12 * HOUR,
        store: Vec::new(),
        errors: vec![ProjectErrors {
            slug: "api".to_owned(),
            created: crate::time::midnight(first),
            first,
            daily: (0..10)
                .map(|i| {
                    let records = 100_000;
                    let d = first.checked_add(jiff::Span::new().days(i)).unwrap();
                    (
                        d,
                        Count {
                            records,
                            bytes: records * 2_000,
                        },
                    )
                })
                .collect(),
            stored: 1_000_000,
            days: 30,
        }],
        prior: FIXTURE,
    };
    let data = Path::new("/var/lib/davy");
    let said = "40 GiB free, forecast full on 2026-11-25, in 0 days";
    let fix = "shorten retention with davy project retention SLUG --data /var/lib/davy --errors N, or add disk";
    for raw in [
        Vec::new(),
        vec![RawDay {
            day: day("2026-11-25"),
            projects: vec![("api".to_owned(), 1_869, false)],
        }],
    ] {
        input.raw = raw;
        let f = forecast(&input, &WhatIf::default());
        let check = disk(data, Some(40 * GIB), Some(f));
        assert_eq!(
            (check.status, check.detail.as_str(), check.fix.as_deref()),
            (Status::Fail, said, Some(fix)),
            "{:?}",
            input.raw
        );
    }
}
