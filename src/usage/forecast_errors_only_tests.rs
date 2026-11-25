//! A server whose logs add nothing: no raw log bytes counted and no
//! closed log day on disk. Its forecast rests on errors alone.

use super::*;
use crate::{time, usage::Count, usage::footprint::FIXTURE};

const GB: u64 = 1_000_000_000;
const MB: u64 = 1_000_000;

pub(super) fn day(s: &str) -> Date {
    s.parse().unwrap()
}

/// 100 000 events a day of 2 000 bytes as sent from 2026-11-17 to
/// 2026-11-23, by the footprint benchmark 600 + 1 000 = 1 600 bytes each
/// in SQLite: 160 MB a day. The project keeps 30 days of them, 3 000 000,
/// and holds 1 000 000, so 2 000 000 more of 1 600 bytes add 3.2 GB. The
/// disk is 1 GB short of its limit of 450 GB.
pub(super) fn errors_only() -> Input {
    let first = day("2026-11-17");
    Input {
        today: day("2026-11-24"),
        closed: Vec::new(),
        davy: 3 * GB,
        fixed: 3 * GB,
        used: 449 * GB,
        total: 500 * GB,
        projects: vec![("api".to_owned(), 30)],
        default_days: 30,
        raw: Vec::new(),
        counted_since: Some(time::midnight(day("2026-10-01"))),
        now: time::midnight(day("2026-11-24")) + 12 * time::HOUR_NS,
        store: Vec::new(),
        errors: vec![ProjectErrors {
            slug: "api".to_owned(),
            created: time::midnight(day("2026-09-01")),
            first: day("2026-09-01"),
            daily: (0..7)
                .map(|i| (first.checked_add(i.days()).unwrap(), events(100_000)))
                .collect(),
            stored: 1_000_000,
            days: 30,
        }],
        prior: FIXTURE,
    }
}

/// `n` events of 2 000 bytes as sent.
pub(super) fn events(n: u64) -> Count {
    Count {
        records: n,
        bytes: n * 2_000,
    }
}

/// SQLite sampled once a day from 2026-11-16, growing by 100 000 events a
/// day at each of `per_event` bytes in turn.
fn store(per_event: &[u64]) -> Vec<StoreDay> {
    let first = day("2026-11-16");
    let mut bytes = 3 * GB;
    let mut out = vec![StoreDay {
        day: first,
        at_ms: time::midnight(first) / 1_000_000,
        bytes,
        events: 1_000_000,
    }];
    for (i, b) in (1..).zip(per_event) {
        let d = first.checked_add(i.days()).unwrap();
        bytes += b * 100_000;
        out.push(StoreDay {
            day: d,
            at_ms: time::midnight(d) / 1_000_000,
            bytes,
            events: 1_000_000 + 100_000 * u64::try_from(i).unwrap(),
        });
    }
    out
}

const WHY: &str = "Davy counted no raw log bytes in the last 30 days and the disk holds no closed log day since 2026-10-25, so logs add nothing and the forecast rests on errors alone";

/// Bytes per event from the benchmark alone: the bound and the estimate
/// take the same 1 600 bytes, so the range is a single day.
#[test]
fn errors_on_the_benchmark_give_a_medium_forecast() {
    let f = forecast(&errors_only(), &WhatIf::default()).unwrap();
    let full = Some(day("2026-12-01"));
    assert_eq!(
        f,
        Forecast {
            growth: 0,
            errors: Some(Errors {
                projects: 1,
                events_per_day: 100_000,
                bytes_per_event: 1_600,
                own_days: 0,
                basis: ErrorBasis::Footprint,
                growth: 160 * MB,
                estimate_growth: 160 * MB,
                // Errors are all that grows.
                share_ppm: 1_000_000,
            }),
            longest_days: 30,
            shortest_days: 30,
            // 3 GB of SQLite and uploads and 3.2 GB more of errors.
            steady_state: 6_200 * MB,
            headroom: 50 * GB,
            // 1 GB at 160 MB a day: 6.25, so 7.
            days_left: Some(7),
            full_on: full,
            beyond_10_years: false,
            unseen: None,
            logs: Some(Logs {
                basis: Basis::ErrorsOnly {
                    lean: Lean::Benchmark(0),
                    logs_pending: false,
                },
                growth: Spread::flat(0),
                own_days: 0,
                possible_days: 7,
                // 7 whole days counted, each of 0 raw bytes.
                raw: Some(Spread::flat(0)),
                raw_days: 7,
                counted_ns: None,
                recent: false,
                ratio: None,
                own_ratio: None,
                prior: FIXTURE,
                why: WHY.to_owned(),
            }),
            full_on_range: [full, full],
            estimate: Some(Estimate {
                steady_state: 6_200 * MB,
                fill: Fill {
                    days_left: Some(7),
                    full_on: full,
                    beyond_10_years: false,
                },
                clamped: false,
            }),
        }
    );
    assert_eq!(f.confidence(false), Some(Confidence::Medium));
    assert_eq!(
        f.explain(false).unwrap(),
        format!(
            "{WHY}; confidence is medium as SQLite growth measured bytes per event on 0 of 7 days and the footprint benchmark gives the rest"
        )
    );
    assert!(!f.worst_case());
    assert_eq!(
        f.estimate_takes(),
        "at the middle bytes per event of errors"
    );
}

/// 7 days measured 1 000 to 3 000 bytes per event: the estimate takes the
/// median, 2 000, and the bound the high end, the second highest, 3 000.
#[test]
fn errors_measured_on_7_days_give_a_high_forecast() {
    let input = Input {
        store: store(&[1_000, 1_000, 1_500, 2_000, 2_500, 3_000, 3_000]),
        ..errors_only()
    };
    let f = forecast(&input, &WhatIf::default()).unwrap();
    let e = f.errors.unwrap();
    assert_eq!(
        (
            e.basis,
            e.own_days,
            e.bytes_per_event,
            e.growth,
            e.estimate_growth
        ),
        (ErrorBasis::Measured, 7, 2_000, 300 * MB, 200 * MB)
    );
    // 3 GB, and 2 000 000 events more at 3 000 bytes: 6 GB.
    assert_eq!(f.steady_state, 9 * GB);
    // 1 GB at 300 MB a day: 3.3, so 4. At 200 MB: 5.
    assert_eq!((f.days_left, f.full_on), (Some(4), Some(day("2026-11-28"))));
    assert_eq!(
        f.full_on_range,
        [Some(day("2026-11-28")), Some(day("2026-11-29"))]
    );
    let estimate = f.estimate.unwrap();
    // 3 GB, and 2 000 000 events more at 2 000 bytes: 4 GB.
    assert_eq!(
        (
            estimate.steady_state,
            estimate.fill.full_on,
            estimate.clamped
        ),
        (7 * GB, Some(day("2026-11-29")), false)
    );
    assert_eq!(
        f.logs.as_ref().unwrap().basis,
        Basis::ErrorsOnly {
            lean: Lean::Measured,
            logs_pending: false,
        }
    );
    assert_eq!(f.confidence(false), Some(Confidence::High));
    assert_eq!(f.explain(false).unwrap(), WHY);
    // The disk walk leaving something out lowers it as for logs.
    assert_eq!(f.confidence(true), Some(Confidence::Low));
    assert_eq!(
        f.explain(true).unwrap(),
        format!("{WHY}; confidence is low as the disk walk left something out")
    );
}

/// A project made at 06:00 today: 25 000 events in 6 hours are 100 000 a
/// day, of 1 600 bytes each. It holds 25 000, so 2 975 000 more add
/// 4.76 GB.
#[test]
fn errors_that_began_today_give_a_low_forecast() {
    let today = day("2026-11-24");
    let input = Input {
        errors: vec![ProjectErrors {
            slug: "api".to_owned(),
            created: time::midnight(today) + 6 * time::HOUR_NS,
            first: today,
            daily: vec![(today, events(25_000))],
            stored: 25_000,
            days: 30,
        }],
        ..errors_only()
    };
    let f = forecast(&input, &WhatIf::default()).unwrap();
    let e = f.errors.unwrap();
    assert_eq!((e.events_per_day, e.growth), (100_000, 160 * MB));
    assert_eq!(f.steady_state, 7_760 * MB);
    assert_eq!((f.days_left, f.full_on), (Some(7), Some(day("2026-12-01"))));
    assert_eq!(
        f.logs.as_ref().unwrap().basis,
        Basis::ErrorsOnly {
            lean: Lean::Hours,
            logs_pending: false,
        }
    );
    assert_eq!(f.confidence(false), Some(Confidence::Low));
    assert_eq!(
        f.explain(false).unwrap(),
        format!(
            "{WHY}; confidence is low as errors that began less than a whole day ago add over 5% of the errors' growth, so it rests on hours"
        )
    );
}

/// Errors since 2026-11-21: 3 whole days of 100 000 events.
#[test]
fn errors_that_began_this_week_give_a_low_forecast() {
    let first = day("2026-11-21");
    let input = Input {
        errors: vec![ProjectErrors {
            slug: "api".to_owned(),
            created: time::midnight(first),
            first,
            daily: (0..3)
                .map(|i| (first.checked_add(i.days()).unwrap(), events(100_000)))
                .collect(),
            stored: 1_000_000,
            days: 30,
        }],
        ..errors_only()
    };
    let f = forecast(&input, &WhatIf::default()).unwrap();
    assert_eq!(f.errors.unwrap().growth, 160 * MB);
    assert_eq!((f.days_left, f.full_on), (Some(7), Some(day("2026-12-01"))));
    assert_eq!(
        f.logs.as_ref().unwrap().basis,
        Basis::ErrorsOnly {
            lean: Lean::Days,
            logs_pending: false,
        }
    );
    assert_eq!(f.confidence(false), Some(Confidence::Low));
    assert_eq!(
        f.explain(false).unwrap(),
        format!(
            "{WHY}; confidence is low while errors that began less than 7 whole days ago add over 5% of the errors' growth"
        )
    );
}

/// Errors older than 7 days add nothing to the rate: nothing grows.
#[test]
fn errors_older_than_a_week_and_no_logs_give_no_forecast() {
    let mut input = errors_only();
    input.errors[0].daily = vec![(day("2026-11-10"), events(100_000))];
    assert_eq!(
        forecast(&input, &WhatIf::default()),
        Err(NoForecast::History(
            "nothing grows yet: Davy counted no raw log bytes in the last 30 days, the disk holds no closed log day since 2026-10-25, and no project received error events in the last 7 days".to_owned()
        ))
    );
}

/// 100 MB of logs a day on top of the errors' 160 MB. The logs take
/// 3.1 GB for 30 closed days and today, the errors 3.2 GB after 20 days:
/// 1 GB at 260 MB a day is 3.8 days, so 4.
#[test]
fn a_what_if_of_log_bytes_adds_them_to_the_errors() {
    let what_if = WhatIf {
        logs_bytes_per_day: Some(100 * MB),
        ..WhatIf::default()
    };
    let f = forecast(&errors_only(), &what_if).unwrap();
    let full = Some(day("2026-11-28"));
    assert_eq!(
        (f.growth, f.days_left, f.full_on, f.full_on_range),
        (100 * MB, Some(4), full, [full, full])
    );
    assert_eq!(f.steady_state, 9_300 * MB);
    // In 4 days logs add 400 MB and errors 640 MB: 640 of 1 040.
    assert_eq!(f.errors.unwrap().share_ppm, 615_385);
    assert_eq!(
        (f.logs.as_ref(), f.estimate, f.confidence(false)),
        (None, None, None)
    );
}

/// `api` of [`errors_only`] at `old` events a day, and `new`.
fn with_new(old: u64, new: ProjectErrors) -> Input {
    let mut input = errors_only();
    let first = day("2026-11-17");
    input.errors[0].daily = (0..7)
        .map(|i| (first.checked_add(i.days()).unwrap(), events(old)))
        .collect();
    input.errors.push(new);
    input
}

/// A project `new` whose errors began at `began`, with `count` events that
/// day.
fn new_project(began: &str, count: u64) -> ProjectErrors {
    let at = time::nanos(time::parse(began).unwrap()).unwrap();
    let first = time::day_of(at);
    ProjectErrors {
        slug: "new".to_owned(),
        created: at,
        first,
        daily: vec![(first, events(count))],
        stored: count,
        days: 30,
    }
}

/// Errors that began this week or today lower the confidence only above
/// 5% of the errors' growth. All events pack alike, so the share is that
/// of events: 5 000 of 100 000 is 50 000 ppm, not over; 5 000 of 99 999
/// is 50 000.5, rounded up to 50 001. 1 250 events in the 6 hours since
/// 06:00 are 5 000 a day, and 15 000 in the 3 days since 2026-11-21 too.
#[test]
fn recent_errors_lower_the_confidence_only_above_5_percent_of_the_growth() {
    let today = "2026-11-24T06:00:00Z";
    let week = |count| {
        let mut p = new_project("2026-11-21T00:00:00Z", 0);
        p.daily = (0..3)
            .map(|i| {
                (
                    day("2026-11-21").checked_add(i.days()).unwrap(),
                    events(count),
                )
            })
            .collect();
        p
    };
    for (old, new, lean) in [
        (95_000, new_project(today, 1_250), Lean::Benchmark(0)),
        (94_999, new_project(today, 1_250), Lean::Hours),
        (95_000, week(5_000), Lean::Benchmark(0)),
        (94_999, week(5_000), Lean::Days),
    ] {
        let f = forecast(&with_new(old, new), &WhatIf::default()).unwrap();
        let e = f.errors.unwrap();
        assert_eq!(
            (e.events_per_day, f.logs.unwrap().basis),
            (
                old + 5_000,
                Basis::ErrorsOnly {
                    lean,
                    logs_pending: false
                }
            ),
            "{old} {lean:?}"
        );
    }
}

/// One new project with one event, made at 06:00 today or on 2026-11-22,
/// next to api's 100 000 a day: 4 a day from 6 hours, or 1 a day from 2
/// days, leave the confidence medium.
#[test]
fn one_new_project_with_one_event_leaves_the_confidence() {
    for (began, per_day) in [
        ("2026-11-24T06:00:00Z", 100_004),
        ("2026-11-22T00:00:00Z", 100_001),
    ] {
        let f = forecast(
            &with_new(100_000, new_project(began, 1)),
            &WhatIf::default(),
        )
        .unwrap();
        assert_eq!(f.errors.unwrap().events_per_day, per_day, "{began}");
        assert_eq!(f.confidence(false), Some(Confidence::Medium), "{began}");
        assert_eq!(
            f.explain(false).unwrap(),
            format!(
                "{WHY}; confidence is medium as SQLite growth measured bytes per event on 0 of 7 days and the footprint benchmark gives the rest"
            ),
            "{began}"
        );
    }
}

/// Logs count as 0, so a log retention, for one project of two or for
/// all, moves nothing without log days on disk to delete, and makes no
/// worst case.
#[test]
fn a_log_retention_what_if_moves_nothing_on_errors_alone() {
    let mut input = errors_only();
    input.projects.push(("new".to_owned(), 30));
    for project in [Some("new".to_owned()), None] {
        let what_if = WhatIf {
            project: project.clone(),
            logs_days: Some(7),
            ..WhatIf::default()
        };
        let f = forecast(&input, &what_if).unwrap();
        assert_eq!(
            (f.days_left, f.full_on, f.worst_case()),
            (Some(7), Some(day("2026-12-01")), false),
            "{project:?}"
        );
        assert_eq!(
            f.unseen.as_deref(),
            Some(
                "this forecast counts logs as 0, as it rests on errors alone, so a log retention changes it only by the log days on disk it deletes"
            ),
            "{project:?}"
        );
    }
}
