//! Errors in the bound and the estimate, and what they do to the
//! confidence, on the server of [`super::tests`].

use super::tests::{GB, MB, both, day, server, shop_errors};
use crate::{
    time,
    usage::{
        Count,
        forecast::{
            Confidence, ErrorBasis, Errors, Input, ProjectErrors, StoreDay, WhatIf, forecast,
        },
    },
};
use jiff::ToSpan as _;

#[test]
fn errors_fill_a_disk_the_logs_alone_do_not() {
    // 32 GB to the limit: the bound's logs have 29.6 GB of room and never
    // reach it.
    let input = Input {
        used: 418 * GB,
        ..server()
    };
    let f = forecast(&input, &WhatIf::default()).unwrap();
    assert_eq!((f.days_left, f.full_on), (None, None));
    // shop's errors add 210 MB a day: 3.81 GB a day until the logs are
    // full on day 9, then 2.4 GB more at 210 MB a day.
    let input = Input {
        errors: vec![shop_errors()],
        ..input
    };
    let f = forecast(&input, &WhatIf::default()).unwrap();
    assert_eq!(
        (f.days_left, f.full_on),
        (Some(12), Some(day("2026-12-06")))
    );
}

#[test]
fn errors_add_to_the_bound_and_the_estimate() {
    let input = Input {
        errors: vec![shop_errors()],
        ..server()
    };
    let f = forecast(&input, &WhatIf::default()).unwrap();
    // 2,100 B an event from the footprint: 210 MB a day, and 1.8 million
    // events to go before retention deletes as many, 3.78 GB.
    assert_eq!(
        f.errors.unwrap(),
        Errors {
            projects: 1,
            events_per_day: 100_000,
            bytes_per_event: 2_100,
            own_days: 0,
            basis: ErrorBasis::Footprint,
            growth: 210 * MB,
            estimate_growth: 210 * MB,
            // In the bound's 6 days, errors add 1.26 GB and logs 21.6
            // GB: 5.5118%.
            share_ppm: 55_119,
        }
    );
    // The bound: 20 GB at 3.81 GB a day. The estimate: 2.04 GB of logs
    // and 3.78 GB of errors are less than 20.
    assert_eq!(
        both(&f),
        [
            (117_380 * MB, Some(6), Some(day("2026-11-30"))),
            (89_780 * MB, None, None)
        ]
    );
    // Errors over 5% of what the bound adds, measured on no day: the
    // measured logs alone would be high.
    assert_eq!(f.logs.as_ref().unwrap().confidence(false), Confidence::High);
    assert_eq!(f.confidence(false), Some(Confidence::Medium));
    assert!(f.explain(false).unwrap().ends_with(
        "; confidence is medium as errors add over 5% of what the bound adds until the disk fills, or of the room left to the limit within 10 years where it never fills, and SQLite growth measured their bytes per event on 0 of 7 days"
    ));
    // A tenth of them stays under 5%.
    let mut few = shop_errors();
    few.daily[0].1 = Count {
        records: 70_000,
        bytes: 210_000_000,
    };
    let input = Input {
        errors: vec![few],
        ..server()
    };
    let f = forecast(&input, &WhatIf::default()).unwrap();
    assert_eq!(f.confidence(false), Some(Confidence::High));
    assert!(!f.explain(false).unwrap().contains("errors"));
}

#[test]
fn errors_measured_on_7_days_keep_the_confidence() {
    // shop received 100,000 events a day, and SQLite grew by 200 MB a day
    // with them, sampled 5 minutes past midnight: 2,000 B an event on 7
    // days. Errors add 5.3% of the growth, measured: confidence stays
    // high. The bound takes the footprint's 2,100 B, the higher.
    let first = day("2026-11-17");
    let store = (0..8_u64)
        .map(|i| {
            let d = first.checked_add(i64::try_from(i).unwrap().days()).unwrap();
            StoreDay {
                day: d,
                at_ms: time::midnight(d) / 1_000_000 + 300_000,
                bytes: 900 * MB + i * 200 * MB,
                events: 500_000 + i * 100_000,
            }
        })
        .collect();
    let daily = (0..7)
        .map(|i| {
            let count = Count {
                records: 100_000,
                bytes: 300_000_000,
            };
            (first.checked_add(i.days()).unwrap(), count)
        })
        .collect();
    let input = Input {
        store,
        errors: vec![ProjectErrors {
            daily,
            ..shop_errors()
        }],
        ..server()
    };
    let f = forecast(&input, &WhatIf::default()).unwrap();
    let e = f.errors.unwrap();
    assert_eq!(
        (e.basis, e.own_days, e.estimate_growth, e.growth),
        (ErrorBasis::Measured, 7, 200 * MB, 210 * MB)
    );
    assert_eq!(f.confidence(false), Some(Confidence::High));
    assert!(!f.explain(false).unwrap().contains("errors"));
}

const MEDIUM: &str = "; confidence is medium as errors add over 5% of what the bound adds until the disk fills, or of the room left to the limit within 10 years where it never fills, and SQLite growth measured their bytes per event on 0 of 7 days";

#[test]
fn errors_that_alone_fill_the_disk_lower_the_confidence() {
    // Logs at their steady state: 3.6 GB a day for 31 days is 111.6 GB,
    // and Davy holds 112 GB of logs. shop's 10,000 events a day, kept 100
    // years, fill the 5 GB to the limit at 21 MB a day in 239 days. Their
    // 21 MB are 0.58% of the gross growth but all that the disk fills by.
    let mut shop = shop_errors();
    shop.daily[0].1 = Count {
        records: 70_000,
        bytes: 210_000_000,
    };
    let input = Input {
        davy: 114 * GB,
        used: 445 * GB,
        errors: vec![ProjectErrors {
            days: 36_500,
            ..shop
        }],
        ..server()
    };
    let f = forecast(&input, &WhatIf::default()).unwrap();
    assert_eq!(f.days_left, Some(239));
    assert_eq!(f.errors.unwrap().share_ppm, 1_000_000);
    assert_eq!(f.confidence(false), Some(Confidence::Medium));
    assert!(f.explain(false).unwrap().ends_with(MEDIUM));
    // Logs already low lower nothing more, and say nothing of errors.
    assert_eq!(f.confidence(true), Some(Confidence::Low));
    assert!(!f.explain(true).unwrap().contains("errors"));
}

#[test]
fn errors_at_their_steady_state_keep_the_confidence() {
    // shop holds its 30 days of 100,000 events a day: its errors add
    // nothing, and the bound fills by logs alone in 6 days, as without
    // them.
    let input = Input {
        errors: vec![ProjectErrors {
            stored: 3_000_000,
            ..shop_errors()
        }],
        ..server()
    };
    let f = forecast(&input, &WhatIf::default()).unwrap();
    assert_eq!(f.days_left, Some(6));
    assert_eq!(f.errors.unwrap().share_ppm, 0);
    assert_eq!(f.confidence(false), Some(Confidence::High));
    assert!(!f.explain(false).unwrap().contains("errors"));
}

#[test]
fn errors_lower_the_confidence_only_over_5_percent() {
    // A disk that never fills: 29.9 GB of room for logs, 2.1 GB for
    // shop's 1,000,000 events to go at 2,100 B, and 42 GB to the limit.
    // In 10 years at 210 MB a day errors would add 766.5 GB, so they add
    // their room: 2.1 of the 42 GB, 5% exactly.
    let errors = |stored| ProjectErrors {
        stored,
        ..shop_errors()
    };
    let input = Input {
        davy: 83_700 * MB,
        used: 408 * GB,
        errors: vec![errors(2_000_000)],
        ..server()
    };
    let f = forecast(&input, &WhatIf::default()).unwrap();
    assert_eq!(f.days_left, None);
    assert_eq!(f.errors.unwrap().share_ppm, 50_000);
    assert_eq!(f.confidence(false), Some(Confidence::High));
    // One event more to go: 2,100,002,100 of 42,000,000,000 B, rounded up.
    let input = Input {
        errors: vec![errors(1_999_999)],
        ..input
    };
    let f = forecast(&input, &WhatIf::default()).unwrap();
    assert_eq!(f.days_left, None);
    assert_eq!(f.errors.unwrap().share_ppm, 50_001);
    assert_eq!(f.confidence(false), Some(Confidence::Medium));
    assert!(f.explain(false).unwrap().ends_with(MEDIUM));
}

#[test]
fn a_trickle_of_errors_on_a_disk_that_never_fills_keeps_the_confidence() {
    // shop's 10 events a day at 2,100 B, kept 100 years with 1,000 kept
    // now: 764.4 MB of room, of which 10 years add 21,000 B x 3,650 =
    // 76.65 MB. 150 GB are to the limit, so they are 0.0511% of it,
    // whatever room the logs have left: none, 20 GB after a quiet week,
    // or 1.6 GB.
    let mut shop = shop_errors();
    shop.daily[0].1 = Count {
        records: 70,
        bytes: 210_000,
    };
    let shop = ProjectErrors {
        stored: 1_000,
        days: 36_500,
        ..shop
    };
    for davy in [114 * GB, 93_600 * MB, 112 * GB] {
        let input = Input {
            davy,
            used: 300 * GB,
            errors: vec![shop.clone()],
            ..server()
        };
        let f = forecast(&input, &WhatIf::default()).unwrap();
        let e = f.errors.unwrap();
        assert_eq!(
            (f.days_left, f.beyond_10_years, e.growth, e.share_ppm),
            (None, false, 21_000, 511),
            "{davy}"
        );
        assert_eq!(f.confidence(false), Some(Confidence::High), "{davy}");
        assert!(!f.explain(false).unwrap().contains("errors"), "{davy}");
    }
}

#[test]
fn the_bound_keeps_large_payloads_at_or_above_the_estimate() {
    // SQLite measured 7 days of 20,000 events, median 1,820 B an event
    // and second highest 1,900. api's 10,000 a day of 200 B are 700 B by
    // the footprint, shop's of 3 KB 2,100: a mean of 1,400. The estimate
    // gives shop 1,820 x 2,100 / 1,400 = 2,730 B, the bound 1,900 x
    // 2,100 / 1,400 = 2,850, above the high end and the footprint. api
    // holds its 30 days, logs hold theirs, and 5 GB are to the limit.
    let first = day("2026-11-17");
    let per_event: [u64; 7] = [1_800, 1_820, 1_950, 1_820, 1_840, 1_900, 1_820];
    let mut store = vec![StoreDay {
        day: first,
        at_ms: time::midnight(first) / 1_000_000 + 300_000,
        bytes: 900 * MB,
        events: 500_000,
    }];
    for (i, bytes) in (1..).zip(per_event) {
        let last = *store.last().unwrap();
        let d = first.checked_add(i.days()).unwrap();
        store.push(StoreDay {
            day: d,
            at_ms: time::midnight(d) / 1_000_000 + 300_000,
            bytes: last.bytes + bytes * 20_000,
            events: last.events + 20_000,
        });
    }
    let week = |records, bytes| vec![(day("2026-11-19"), Count { records, bytes })];
    let api = ProjectErrors {
        slug: "api".to_owned(),
        daily: week(70_000, 14_000_000),
        stored: 300_000,
        ..shop_errors()
    };
    let shop = ProjectErrors {
        daily: week(70_000, 210_000_000),
        days: 36_500,
        ..shop_errors()
    };
    let input = Input {
        davy: 114 * GB,
        used: 445 * GB,
        raw: Vec::new(),
        counted_since: None,
        store,
        errors: vec![api, shop],
        ..server()
    };
    let f = forecast(&input, &WhatIf::default()).unwrap();
    let e = f.errors.unwrap();
    // api adds 19 MB in the bound and 9.1 MB in the estimate, shop 28.5
    // and 27.3.
    assert_eq!(
        (e.own_days, e.growth, e.estimate_growth),
        (7, 47_500_000, 36_400_000)
    );
    // The bound fills the 5 GB at shop's 28.5 MB a day, the estimate at
    // 27.3 MB, later by its own sums.
    assert_eq!(
        (f.days_left, f.estimate.unwrap().fill.days_left),
        (Some(176), Some(184))
    );
    assert!(!f.estimate.unwrap().clamped);
}
