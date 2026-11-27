use super::*;
use crate::{
    time,
    usage::{
        Count,
        footprint::FIXTURE,
        forecast::{Forecast, ProjectErrors, RawDay, Spread, WhatIf, forecast},
    },
};

pub(super) const GB: u64 = 1_000_000_000;
pub(super) const MB: u64 = 1_000_000;

pub(super) fn day(s: &str) -> Date {
    s.parse().unwrap()
}

/// Raw bytes of api and shop on the newest 7 closed days, 2026-11-17 to
/// 2026-11-23: 2 GB and 4 GB a day by both median and mean.
const API: [u64; 7] = [2_000, 2_000, 2_100, 1_900, 2_000, 2_000, 2_000];
const SHOP: [u64; 7] = [4_100, 3_900, 4_000, 4_000, 4_200, 3_800, 4_000];

/// A server counting since 2026-10-20 whose shop keeps logs 30 days and
/// api 7, asked at noon on 2026-11-24. The disk holds shop's 30 closed
/// days, so api's rows before 2026-11-17 are on disk days but cut. Each
/// day older than the 7 has 2 GB of api, cut, and 4 GB of shop; today has
/// 0.9 GB of api and 1.7 GB of shop so far. Logs pack to 0.6 of their raw
/// bytes, 82 GB on disk with today's, and the disk has 20 GB to its
/// limit: 430 GB used of 500, 50 GB headroom.
pub(super) fn server() -> Input {
    let today = day("2026-11-24");
    let first = day("2026-10-25");
    let mut raw = Vec::new();
    let mut closed = Vec::new();
    for i in 0..30 {
        let d = first.checked_add(i64::from(i).days()).unwrap();
        let (api, shop, cut) = match usize::try_from(i).unwrap().checked_sub(23) {
            Some(n) => (API[n] * MB, SHOP[n] * MB, false),
            None => (2 * GB, 4 * GB, true),
        };
        raw.push(RawDay {
            day: d,
            projects: vec![
                ("api".to_owned(), api, cut),
                ("shop".to_owned(), shop, false),
            ],
        });
        let kept = if cut { shop } else { api + shop };
        closed.push((d, kept / 10 * 6));
    }
    raw.push(RawDay {
        day: today,
        projects: vec![
            ("api".to_owned(), 900 * MB, false),
            ("shop".to_owned(), 1_700 * MB, false),
        ],
    });
    Input {
        today,
        closed,
        davy: 84 * GB,
        fixed: 2 * GB,
        used: 430 * GB,
        total: 500 * GB,
        projects: vec![("api".to_owned(), 7), ("shop".to_owned(), 30)],
        default_days: 30,
        cold: Vec::new(),
        hot_days: None,
        raw,
        counted_since: Some(time::midnight(day("2026-10-20"))),
        now: time::midnight(today) + 12 * 3_600_000_000_000,
        store: Vec::new(),
        errors: Vec::new(),
        prior: FIXTURE,
    }
}

/// Measured logs that put 3.6 GB on disk a day: 0.6 of the 6 GB raw.
fn measured(mid: u64) -> Logs {
    Logs {
        basis: Basis::Measured,
        growth: Spread {
            low: mid / 10 * 9,
            mid,
            high: mid / 10 * 11,
        },
        own_days: 7,
        possible_days: 7,
        raw: Some(Spread::flat(6 * GB)),
        raw_days: 7,
        counted_ns: None,
        recent: false,
        ratio: None,
        own_ratio: Some(600_000),
        prior: FIXTURE,
        why: String::new(),
    }
}

#[test]
fn a_raised_retention_does_not_count_cut_days_as_on_disk() {
    let input = server();
    // api's 7 closed days and today: 14 GB and 0.9 GB. Its 23 older
    // days are on disk for shop but cut for api.
    assert_eq!(on_disk(&input, "api", 30), (14_900 * MB, 0));
    assert_eq!(on_disk(&input, "api", 7), (14_900 * MB, 0));
    // shop: 23 older days of 4 GB, 28 GB and 1.7 GB.
    assert_eq!(on_disk(&input, "shop", 30), (121_700 * MB, 0));
    assert_eq!(on_disk(&input, "shop", 7), (29_700 * MB, 0));
    // A day with cold files counts as cold.
    let tiered = Input {
        closed: server().closed[23..].to_vec(),
        cold: server().closed[..23].to_vec(),
        hot_days: Some(7),
        ..server()
    };
    assert_eq!(on_disk(&tiered, "shop", 30), (29_700 * MB, 92_000 * MB));
    // A directory retention already deleted holds nothing.
    let gone = Input {
        closed: server().closed[1..].to_vec(),
        ..server()
    };
    assert_eq!(on_disk(&gone, "shop", 30), (117_700 * MB, 0));
    let split = logs(
        &input,
        &measured(3_600 * MB),
        &[("api", 30), ("shop", 30)],
        all_hot(30),
    );
    assert_eq!(
        split,
        Split {
            parts: vec![
                // 1.2 GB a day for 30 closed days and today, less the
                // 8.94 GB of api's 14.9 GB raw on disk.
                Part {
                    growth: 1_200 * MB,
                    room: 28_260 * MB,
                },
                Part {
                    growth: 2_400 * MB,
                    room: 1_380 * MB,
                },
            ],
            steady: 111_600 * MB,
            freed: 0,
            by_project: true,
        }
    );
}

#[test]
fn a_shorter_retention_frees_what_it_would_delete_now() {
    let input = server();
    let split = logs(
        &input,
        &measured(3_600 * MB),
        &[("api", 7), ("shop", 7)],
        all_hot(7),
    );
    assert_eq!(
        split,
        Split {
            parts: vec![
                Part {
                    growth: 1_200 * MB,
                    room: 660 * MB,
                },
                Part {
                    growth: 2_400 * MB,
                    room: 1_380 * MB,
                },
            ],
            steady: 28_800 * MB,
            // shop's 23 older days, 92 GB raw, are 55.2 GB on disk.
            freed: 55_200 * MB,
            by_project: true,
        }
    );
}

#[test]
fn without_raw_bytes_per_project_the_logs_are_one_part() {
    let input = Input {
        raw: Vec::new(),
        ..server()
    };
    // 3.6 GB a day for 30 closed days and today, less the 82 GB of logs
    // on disk now.
    assert_eq!(
        logs(
            &input,
            &measured(3_600 * MB),
            &[("api", 7), ("shop", 30)],
            all_hot(30)
        ),
        Split {
            parts: vec![Part {
                growth: 3_600 * MB,
                room: 29_600 * MB,
            }],
            steady: 111_600 * MB,
            freed: 0,
            by_project: false,
        }
    );
}

#[test]
fn without_raw_bytes_per_project_a_shorter_retention_frees_its_days_too() {
    // 7 days for all deletes shop's 23 older days, 55.2 GB on disk: 26.8
    // GB of logs stay, 2 GB under the 28.8 GB that 7 closed days and
    // today hold.
    let input = Input {
        raw: Vec::new(),
        ..server()
    };
    assert_eq!(
        logs(
            &input,
            &measured(3_600 * MB),
            &[("api", 7), ("shop", 7)],
            all_hot(7)
        ),
        Split {
            parts: vec![Part {
                growth: 3_600 * MB,
                room: 2_000 * MB,
            }],
            steady: 28_800 * MB,
            freed: 55_200 * MB,
            by_project: false,
        }
    );
    // With shop's errors kept 100 years, the bound and the estimate both
    // fill the 75.2 GB to the limit on the same day, so the estimate
    // takes nothing from the bound: 2 GB of logs, then 210 MB a day.
    let input = Input {
        counted_since: None,
        errors: vec![ProjectErrors {
            days: 36_500,
            ..shop_errors()
        }],
        ..input
    };
    let all = WhatIf {
        logs_days: Some(7),
        ..WhatIf::default()
    };
    let f = forecast(&input, &all).unwrap();
    let full = (7_693_280 * MB, Some(349), Some(day("2027-11-08")));
    assert_eq!(both(&f), [full, full]);
    assert!(!f.estimate.unwrap().clamped);
}

#[test]
fn a_first_day_shares_by_bytes_over_the_time_counted() {
    // 6 hours counted: 0.5 GB of api and 1 GB of shop.
    let input = Input {
        raw: vec![RawDay {
            day: day("2026-11-24"),
            projects: vec![
                ("api".to_owned(), 500 * MB, false),
                ("shop".to_owned(), GB, false),
            ],
        }],
        closed: Vec::new(),
        ..server()
    };
    let first = Logs {
        basis: Basis::FirstDay,
        counted_ns: Some(6 * 3_600_000_000_000),
        ..measured(3_600 * MB)
    };
    assert_eq!(shares(&input, &first), [("api", 2 * GB), ("shop", 4 * GB)]);
    let split = logs(&input, &first, &[("api", 7), ("shop", 30)], all_hot(30));
    assert_eq!(
        split.parts.iter().map(|p| p.growth).collect::<Vec<_>>(),
        [1_200 * MB, 2_400 * MB]
    );
}

#[test]
fn a_quiet_project_shares_by_the_mean_of_its_days() {
    // A project with logs on 3 days of 7: its median is 0, its mean is not.
    let mut input = server();
    for r in &mut input.raw {
        let n = r.day.day();
        for p in &mut r.projects {
            if p.0 == "api" {
                p.1 = if matches!(n, 17 | 20 | 23) { 7 * GB } else { 0 };
            }
        }
    }
    assert_eq!(
        shares(&input, &measured(3_600 * MB)),
        [("api", 3 * GB), ("shop", 4 * GB)]
    );
}

fn what_if(project: &str, days: u32) -> WhatIf {
    WhatIf {
        project: Some(project.to_owned()),
        logs_days: Some(days),
        logs_bytes_per_day: None,
    }
}

/// The bound and the estimate: steady state, days left and day full.
pub(super) fn both(f: &Forecast) -> [(u64, Option<u64>, Option<Date>); 2] {
    let e = f.estimate.unwrap();
    [
        (f.steady_state, f.days_left, f.full_on),
        (e.steady_state, e.fill.days_left, e.fill.full_on),
    ]
}

#[test]
fn a_project_what_if_moves_the_estimate_not_the_bound() {
    // The bound takes api's logs as kept 30 days, as shop keeps them: 20
    // GB in 6 days at 3.6 GB a day. The estimate keeps api's 7 days:
    // with 2 GB of room, it never fills.
    let bound = (113_600 * MB, Some(6), Some(day("2026-11-30")));
    let f = forecast(&server(), &WhatIf::default()).unwrap();
    assert_eq!(both(&f), [bound, (86_000 * MB, None, None)]);
    // api keeping 30 days adds 1.2 GB a day for 31 days, less its
    // 8.94 GB on disk: shop's 1.38 GB of room and 18.62 GB of api's fill
    // the 20 GB in 16 days.
    let f = forecast(&server(), &what_if("api", 30)).unwrap();
    assert_eq!(
        both(&f),
        [bound, (113_600 * MB, Some(16), Some(day("2026-12-10")))]
    );
    assert!(f.unseen.unwrap().contains("The estimate shows it"));
    let f = forecast(&server(), &what_if("api", 3)).unwrap();
    assert_eq!(both(&f), [bound, (81_200 * MB, None, None)]);
}

#[test]
fn a_shorter_retention_of_the_holder_moves_both() {
    // shop at 7 days: the bound keeps 7 too, the estimate frees shop's
    // 55.2 GB of older days at the next sweep.
    let f = forecast(&server(), &what_if("shop", 7)).unwrap();
    assert_eq!(f.unseen, None);
    assert_eq!(
        both(&f),
        [(30_800 * MB, None, None), (30_800 * MB, None, None)]
    );
}

#[test]
fn the_estimate_is_never_earlier_than_the_bound() {
    // A disk with logs the raw bytes do not explain, 30 GB more than
    // the 82 GB they pack to: the bound's room is 30 GB smaller, and it
    // never fills.
    let input = Input {
        davy: 114 * GB,
        used: 445 * GB,
        ..server()
    };
    let f = forecast(&input, &what_if("api", 30)).unwrap();
    assert_eq!(f.days_left, None);
    assert_eq!(f.estimate.unwrap().fill.days_left, None);
    // With shop's errors and 3 GB to the limit, the bound fills by errors
    // alone, 210 MB a day, in 15 days. The estimate's sums put it in 2:
    // shop's 1.38 GB, then 1.41 GB a day. It takes the bound's day.
    let input = Input {
        used: 447 * GB,
        errors: vec![shop_errors()],
        ..input
    };
    let f = forecast(&input, &what_if("api", 30)).unwrap();
    let full = (117_380 * MB, Some(15), Some(day("2026-12-09")));
    assert_eq!(both(&f), [full, full]);
}

#[test]
fn a_lower_longest_retention_frees_its_older_days_in_both() {
    // shop at 20 days, with errors kept 100 years and 3 GB to the limit.
    // The next sweep deletes the 10 closed days before 2026-11-04, 2.4
    // GB each on disk: both free those 24 GB, so 27 GB are to the limit.
    let input = Input {
        used: 447 * GB,
        errors: vec![ProjectErrors {
            days: 36_500,
            ..shop_errors()
        }],
        ..server()
    };
    let f = forecast(&input, &what_if("shop", 20)).unwrap();
    // The bound: 3.6 GB a day for 20 closed days and today, 75.6 GB,
    // less the 58 GB of logs left: 17.6 GB of room, full on day 5. Then
    // errors alone, 210 MB a day: 17.6 + 0.21 t reaches 27 at t = 44.8.
    // Its steady state: 75.6 GB, 2 GB of SQLite and uploads, and
    // 3,648,800,000 events to go at 2,100 B, 7,662.48 GB.
    // The estimate: api's 0.66 GB and shop's 1.38 GB of room, shop
    // freeing 24 GB of its 73.02 GB, then errors: 2.04 + 0.21 t reaches
    // 27 at t = 118.9. Its logs hold 9.6 and 50.4 GB.
    assert_eq!(
        both(&f),
        [
            (7_740_080 * MB, Some(45), Some(day("2027-01-08"))),
            (7_724_480 * MB, Some(119), Some(day("2027-03-23")))
        ]
    );
    assert!(!f.estimate.unwrap().clamped);
    // At 7 days, the bound frees 23 days, 55.2 GB: 2 GB of logs' room and
    // errors fill the 58.2 GB to the limit only on day 268, as the
    // estimate does.
    let f = forecast(&input, &what_if("shop", 7)).unwrap();
    assert_eq!(f.days_left, Some(268));
    assert_eq!(f.estimate.unwrap().fill.days_left, Some(268));
}

#[test]
fn a_lower_longest_retention_frees_only_the_days_it_deletes() {
    // shop at 20 days deletes the 10 closed days before 2026-11-04, 2.4
    // GB each: the day 2026-11-04 itself stays.
    assert_eq!(super::super::freed(&server(), 20), 24 * GB);
    // A day older than shop's 30 that the disk still holds goes at the
    // next sweep whatever the what-if, so 30 days frees nothing.
    let mut input = server();
    input.closed.insert(0, (day("2026-10-24"), GB));
    assert_eq!(super::super::freed(&input, 30), 0);
    // Without projects, the server default of 30 days is what is lowered.
    let input = Input {
        projects: Vec::new(),
        ..server()
    };
    assert_eq!(super::super::freed(&input, 20), 24 * GB);
}

#[test]
fn an_estimate_the_bound_never_fills_is_clamped_to_never() {
    // 1 GB a day with 100 GB of room puts the 20 GB to the limit at day
    // 20 by the estimate's own sums.
    let split = || Split {
        parts: vec![Part {
            growth: GB,
            room: 100 * GB,
        }],
        steady: 50 * GB,
        freed: 0,
        by_project: true,
    };
    let e = |bound| {
        let e = super::super::estimate(&server(), split(), &[], 450 * GB, bound);
        (e.steady_state, e.fill.days_left, e.clamped)
    };
    assert_eq!(e(None), (52 * GB, None, true));
    assert_eq!(e(Some(25)), (52 * GB, Some(25), true));
    assert_eq!(e(Some(20)), (52 * GB, Some(20), false));
}

/// shop received 700,000 error events of 3 KB in one burst this week,
/// 100,000 a day, and keeps 1,200,000 of 30 days' worth.
pub(super) fn shop_errors() -> ProjectErrors {
    ProjectErrors {
        slug: "shop".to_owned(),
        created: time::midnight(day("2026-09-02")) + 37_000_000_000_000,
        first: day("2026-09-02"),
        daily: vec![(
            day("2026-11-19"),
            Count {
                records: 700_000,
                bytes: 2_100_000_000,
            },
        )],
        stored: 1_200_000,
        days: 30,
    }
}

/// `longest` with every day hot.
fn all_hot(longest: u32) -> (u32, Tiers) {
    let tiers = Tiers {
        hot_days: None,
        ratio_ppm: 200_000,
        own: false,
    };
    (longest, tiers)
}
