use super::*;
use crate::usage::{
    footprint::FIXTURE,
    forecast::{WhatIf, forecast},
};

pub(super) const GB: u64 = 1_000_000_000;
pub(super) const MB: u64 = 1_000_000;
pub(super) const HOUR: i64 = 3_600_000_000_000;

pub(super) fn day(s: &str) -> Date {
    s.parse().unwrap()
}

pub(super) fn at(s: &str) -> i64 {
    time::nanos(time::parse(s).unwrap()).unwrap()
}

/// Consecutive days from `first`: raw bytes counted and size on disk.
pub(super) fn days(first: &str, values: &[(u64, Option<u64>)]) -> Vec<(Date, u64, Option<u64>)> {
    let first = day(first);
    values
        .iter()
        .enumerate()
        .map(|(i, (raw, disk))| {
            let i = i64::try_from(i).unwrap();
            (first.checked_add(i.days()).unwrap(), *raw, *disk)
        })
        .collect()
}

/// A server that counts from `since` and is asked at `now`, with project
/// api keeping logs 30 days. The disk has 10 GB to its limit: 440 GB used
/// of 500, 50 GB headroom, and Davy takes 1 GB of logs and 1 GB besides.
pub(super) fn server(since: &str, now: &str, days: &[(Date, u64, Option<u64>)]) -> Input {
    let now = at(now);
    Input {
        today: time::day_of(now),
        closed: days
            .iter()
            .filter_map(|(d, _, disk)| Some((*d, (*disk)?)))
            .collect(),
        davy: GB,
        fixed: GB,
        used: 440 * GB,
        total: 500 * GB,
        projects: vec![("api".to_owned(), 30)],
        default_days: 30,
        cold: Vec::new(),
        hot_days: None,
        raw: days
            .iter()
            .filter(|(_, raw, _)| *raw > 0)
            .map(|(d, raw, _)| RawDay {
                day: *d,
                projects: vec![("api".to_owned(), *raw, false)],
            })
            .collect(),
        counted_since: Some(at(since)),
        now,
        store: Vec::new(),
        errors: Vec::new(),
        prior: FIXTURE,
    }
}

pub(super) fn cut(input: &mut Input, d: &str) {
    for r in input.raw.iter_mut().filter(|r| r.day == day(d)) {
        for p in &mut r.projects {
            p.2 = true;
        }
    }
}

pub(super) fn logs_of(input: &Input) -> Result<Logs, NoForecast> {
    forecast(input, &WhatIf::default()).map(|f| f.logs.unwrap())
}

/// 1 GB raw on each of the 3 days counting has run, own ratios of 600,000,
/// 650,000 and 700,000 ppm.
pub(super) fn three_days() -> Input {
    let three = [
        (GB, Some(600 * MB)),
        (GB, Some(650 * MB)),
        (GB, Some(700 * MB)),
    ];
    server(
        "2026-11-21T00:00:00Z",
        "2026-11-24T12:00:00Z",
        &days("2026-11-21", &three),
    )
}

#[test]
fn three_of_seven_days_blend_with_the_footprint() {
    let l = logs_of(&three_days()).unwrap();
    assert_eq!(
        l,
        Logs {
            basis: Basis::Blended,
            growth: Spread {
                low: 579_200_000,
                mid: 713_428_000,
                high: 873_772_000,
            },
            own_days: 3,
            possible_days: 7,
            raw: Some(Spread::flat(GB)),
            raw_days: 3,
            counted_ns: None,
            recent: true,
            ratio: Some(Band {
                low: 579_200,
                mid: 713_428,
                high: 873_772,
            }),
            own_ratio: Some(650_000),
            prior: FIXTURE,
            why: "3 of 7 closed log days measured on this server in the last 30; the rest of the ratio comes from the footprint benchmark".to_owned(),
        }
    );
}

#[test]
fn a_three_day_server_is_measured_at_three_days() {
    let three = [(GB, Some(GB)); 3];
    let mut input = server(
        "2026-11-21T00:00:00Z",
        "2026-11-24T12:00:00Z",
        &days("2026-11-21", &three),
    );
    input.projects[0].1 = 3;
    let l = logs_of(&input).unwrap();
    assert_eq!(
        (l.basis, l.growth, l.own_days, l.possible_days, l.ratio),
        (
            Basis::Measured,
            Spread::flat(GB),
            3,
            3,
            Some(Band {
                low: 1_000_000,
                mid: 1_000_000,
                high: 1_000_000,
            })
        )
    );
}

#[test]
fn the_first_day_rate_divides_by_hours_counted() {
    let input = server(
        "2026-11-24T18:00:00Z",
        "2026-11-24T19:00:00Z",
        &days("2026-11-24", &[(GB, None)]),
    );
    let f = forecast(&input, &WhatIf::default()).unwrap();
    let l = f.logs.clone().unwrap();
    // The bound decides on the high growth: 10 GB to the limit is 1 day.
    assert_eq!((f.growth, f.days_left), (72_295_832_556, Some(1)));
    assert_eq!(
        (l.basis, l.raw, l.growth, l.counted_ns, l.raw_days),
        (
            Basis::FirstDay,
            Some(Spread {
                low: 8 * GB,
                mid: 24 * GB,
                high: 72 * GB,
            }),
            Spread {
                low: 4_509_432_556,
                mid: 18_264_632_556,
                high: 72_295_832_556,
            },
            Some(HOUR),
            0,
        )
    );
    assert_eq!(l.ratio, Some(FIXTURE.ratio));
}

#[test]
fn the_first_day_rate_carries_over_midnight() {
    // The partial day closed with 6 GB on disk, too few days for the
    // direct forecast.
    let two = [(6 * GB, Some(6 * GB)), (GB / 2, None)];
    let input = server(
        "2026-11-23T18:00:00Z",
        "2026-11-24T00:30:00Z",
        &days("2026-11-23", &two),
    );
    let l = logs_of(&input).unwrap();
    assert_eq!(
        (l.basis, l.raw.map(|r| r.mid), l.growth.mid),
        (Basis::FirstDay, Some(24 * GB), 18_264_632_556)
    );
    assert!(forecast(&input, &WhatIf::default()).is_ok());
}

#[test]
fn a_full_day_counted_narrows_the_first_day_range() {
    // Counting started a second after midnight, so 2026-11-23 is not
    // whole, and 24 GB arrived in exactly 24 hours.
    let two = [(23 * GB, Some(10 * GB)), (GB, None)];
    let input = server(
        "2026-11-23T00:00:01Z",
        "2026-11-24T00:00:01Z",
        &days("2026-11-23", &two),
    );
    let l = logs_of(&input).unwrap();
    assert_eq!(
        (l.basis, l.counted_ns, l.raw),
        (
            Basis::FirstDay,
            Some(24 * HOUR),
            Some(Spread {
                low: 16 * GB,
                mid: 24 * GB,
                high: 36 * GB,
            })
        )
    );
}

/// A server that stores neither logs nor errors has no forecast on its
/// first day either, rather than "never" then and none from its third day
/// on, and the reason says nothing grows.
#[test]
fn a_server_without_logs_or_errors_has_no_forecast_on_any_day() {
    for (now, first) in [
        ("2026-11-20T20:00:00Z", "2026-10-21"),
        ("2026-11-23T12:00:00Z", "2026-10-24"),
    ] {
        let input = server("2026-11-20T18:00:00Z", now, &[]);
        assert_eq!(
            forecast(&input, &WhatIf::default()),
            Err(NoForecast::History(format!(
                "nothing grows yet: Davy counted no raw log bytes in the last 30 days, the disk holds no closed log day since {first}, and no project received error events in the last 7 days"
            ))),
            "{now}"
        );
    }
}

#[test]
fn under_an_hour_counted_gives_no_forecast() {
    for (now, minutes) in [("2026-11-24T18:00:00Z", 0), ("2026-11-24T18:59:59Z", 59)] {
        let input = server(
            "2026-11-24T18:00:00Z",
            now,
            &days("2026-11-24", &[(GB, None)]),
        );
        assert_eq!(
            forecast(&input, &WhatIf::default()),
            Err(NoForecast::History(format!(
                "Davy has counted raw log bytes for {minutes} minutes; a first forecast needs an hour of them, or closed log days on disk"
            )))
        );
    }
}

#[test]
fn a_day_under_10_mb_does_not_count_for_the_ratio() {
    for (raw, own) in [(MIN_RAW - 1, 2), (MIN_RAW, 3)] {
        let three = [(GB, Some(GB)), (raw, Some(GB)), (GB, Some(GB))];
        let input = server(
            "2026-11-21T00:00:00Z",
            "2026-11-24T12:00:00Z",
            &days("2026-11-21", &three),
        );
        assert_eq!(logs_of(&input).unwrap().own_days, own, "raw {raw}");
    }
}

#[test]
fn a_small_server_keeps_the_direct_forecast() {
    let week: Vec<(u64, Option<u64>)> = [5, 6, 7, 8, 9, 6, 7]
        .iter()
        .map(|n| (MB, Some(n * MB)))
        .collect();
    let input = server(
        "2026-11-01T00:00:00Z",
        "2026-11-24T12:00:00Z",
        &days("2026-11-17", &week),
    );
    let f = forecast(&input, &WhatIf::default()).unwrap();
    let l = f.logs.unwrap();
    assert_eq!(
        (l.basis, l.growth, f.growth, l.own_days, l.raw, l.raw_days),
        (
            Basis::MeasuredDisk,
            Spread::flat(7 * MB),
            7 * MB,
            0,
            Some(Spread::flat(MB)),
            7
        )
    );
    assert_eq!(
        l.why,
        "the growth is the median size on disk of 7 closed log days: no whole closed day could measure how logs pack, as each had under 10 MB of raw logs, a project that retention cut, or no size on disk"
    );
}

#[test]
fn a_day_with_a_cut_project_does_not_count_for_the_ratio() {
    for (cut_day, own) in [(None, 3), (Some("2026-11-22"), 2)] {
        let mut input = three_days();
        if let Some(d) = cut_day {
            cut(&mut input, d);
        }
        let l = logs_of(&input).unwrap();
        // R counts cut days, the ratio does not.
        assert_eq!((l.own_days, l.raw_days), (own, 3), "{cut_day:?}");
    }
}

#[test]
fn the_partial_first_day_is_never_whole() {
    // The first day has a ratio of 5,000,000 ppm, the others 700,000.
    let four = [
        (GB, Some(5 * GB)),
        (GB, Some(700 * MB)),
        (GB, Some(700 * MB)),
        (GB, Some(700 * MB)),
    ];
    for (since, own, own_ratio) in [
        ("2026-11-20T18:00:00Z", 3, 700_000),
        ("2026-11-20T00:00:01Z", 3, 700_000),
        ("2026-11-20T00:00:00Z", 4, 700_000),
    ] {
        let input = server(since, "2026-11-24T12:00:00Z", &days("2026-11-20", &four));
        let l = logs_of(&input).unwrap();
        assert_eq!(
            (l.basis, l.own_days, l.raw_days, l.own_ratio),
            (Basis::Blended, own, own, Some(own_ratio)),
            "{since}"
        );
    }
}

#[test]
fn an_upgraded_server_keeps_the_direct_forecast_until_its_own_days_catch_up() {
    // 7 closed days of 8 GB on disk and 10 GB raw from 2026-11-17 on.
    let week = days("2026-11-17", &[(10 * GB, Some(8 * GB)); 7]);
    for (since, basis, own, growth) in [
        ("2026-11-21T09:00:00Z", Basis::MeasuredDisk, 2, 8 * GB),
        ("2026-11-18T00:00:00Z", Basis::MeasuredDisk, 6, 8 * GB),
        ("2026-11-17T00:00:00Z", Basis::Measured, 7, 8 * GB),
    ] {
        // The old version wrote no `log_days` rows for its days.
        let mut input = server(since, "2026-11-24T12:00:00Z", &week);
        input.raw.retain(|r| r.day >= time::day_of(at(since)));
        let l = logs_of(&input).unwrap();
        assert_eq!(
            (l.basis, l.own_days, l.growth.mid),
            (basis, own, growth),
            "{since}"
        );
    }
    // A server that never counted keeps the direct forecast too.
    let mut never = server("2026-11-17T00:00:00Z", "2026-11-24T12:00:00Z", &week);
    never.counted_since = None;
    never.raw.clear();
    let l = logs_of(&never).unwrap();
    assert_eq!((l.basis, l.own_days, l.raw), (Basis::MeasuredDisk, 0, None));
}

#[test]
fn an_upgraded_server_blends_once_the_disk_has_no_older_day() {
    // Only the 6 counted days are on disk: the direct method has no day
    // more than the ratio.
    let six = days("2026-11-18", &[(10 * GB, Some(8 * GB)); 6]);
    let l = logs_of(&server(
        "2026-11-18T00:00:00Z",
        "2026-11-24T12:00:00Z",
        &six,
    ))
    .unwrap();
    assert_eq!((l.basis, l.own_days), (Basis::Blended, 6));
}

#[test]
fn quiet_weekend_days_are_skipped_within_30_days() {
    // 2026-10-24 is a Saturday, 31 days before 2026-11-24.
    let from = day("2026-10-24");
    let busy_weekdays = |d: Date| d.weekday().to_monday_one_offset() <= 5;
    // Only 6 busy days in the newest 30, and one 31 days back.
    let busy_six = |d: Date| d == from || d >= day("2026-11-18");
    for (busy, basis, own) in [
        (&busy_weekdays as &dyn Fn(Date) -> bool, Basis::Measured, 7),
        (&busy_six as &dyn Fn(Date) -> bool, Basis::Blended, 6),
    ] {
        let mut all = Vec::new();
        for i in 0..31 {
            let d = from.checked_add(i.days()).unwrap();
            let raw = if busy(d) { GB } else { MB };
            all.push((d, raw, Some(700 * raw / 1000)));
        }
        let l = logs_of(&server(
            "2026-10-01T00:00:00Z",
            "2026-11-24T12:00:00Z",
            &all,
        ))
        .unwrap();
        assert_eq!((l.basis, l.own_days), (basis, own));
        if basis == Basis::Measured {
            // The newest 7 whole days hold the weekend: 5 busy days set
            // the median, and the range drops one quiet day of the two.
            assert_eq!(
                l.growth,
                Spread {
                    low: 700_000,
                    mid: 700 * MB,
                    high: 700 * MB,
                }
            );
        }
    }
}

#[test]
fn blend_and_median_differ_little_on_steady_input() {
    let week = days("2026-11-17", &[(GB, Some(700 * MB)); 7]);
    let mut six = server("2026-11-17T00:00:00Z", "2026-11-24T12:00:00Z", &week);
    cut(&mut six, "2026-11-20");
    let seven = server("2026-11-17T00:00:00Z", "2026-11-24T12:00:00Z", &week);
    let (b, m) = (logs_of(&six).unwrap(), logs_of(&seven).unwrap());
    assert_eq!(
        (b.basis, b.own_days, b.growth.mid, m.basis, m.growth.mid),
        (Basis::Blended, 6, 708_714_000, Basis::Measured, 700_000_000)
    );
    assert!((b.growth.mid - m.growth.mid) * 50 < m.growth.mid);
}

#[test]
fn the_range_drops_one_extreme_day_with_five_or_more() {
    for (disks, retention, range) in [
        (vec![1, 7, 7, 7, 7, 7, 20], 30, (700, 700, 700)),
        (vec![1, 6, 7, 7, 8, 20], 6, (600, 700, 800)),
        (vec![1, 6, 7, 8, 20], 5, (600, 700, 800)),
        (vec![1, 6, 8, 20], 4, (100, 700, 2000)),
    ] {
        let values: Vec<(u64, Option<u64>)> =
            disks.iter().map(|n| (GB, Some(n * 100 * MB))).collect();
        let first = day("2026-11-24").checked_sub(i64::try_from(values.len()).unwrap().days());
        let list = days(&first.unwrap().to_string(), &values);
        let mut input = server("2026-11-01T00:00:00Z", "2026-11-24T12:00:00Z", &list);
        input.projects[0].1 = retention;
        let l = logs_of(&input).unwrap();
        let (low, mid, high) = range;
        assert_eq!(
            (l.basis, l.growth),
            (
                Basis::Measured,
                Spread {
                    low: low * MB,
                    mid: mid * MB,
                    high: high * MB,
                }
            ),
            "{disks:?}"
        );
    }
}

#[test]
fn confidence_is_low_medium_high_by_the_rules() {
    let base = logs_of(&three_days()).unwrap();
    for (basis, recent, partial, want) in [
        (Basis::FirstDay, false, false, Confidence::Low),
        (Basis::Measured, false, true, Confidence::Low),
        (Basis::Measured, true, false, Confidence::Low),
        (Basis::Blended, true, false, Confidence::Low),
        (Basis::MeasuredDisk, true, false, Confidence::Medium),
        (Basis::MeasuredDisk, false, false, Confidence::Medium),
        (Basis::Blended, false, false, Confidence::Medium),
        (Basis::Measured, false, false, Confidence::High),
    ] {
        let l = Logs {
            basis,
            recent,
            ..base.clone()
        };
        assert_eq!(l.confidence(partial), want, "{basis:?} {recent} {partial}");
    }
    // Counting is recent until 7 whole closed days can follow it.
    for (since, recent) in [
        ("2026-11-17T00:00:00Z", false),
        ("2026-11-17T00:00:01Z", true),
    ] {
        let input = server(
            since,
            "2026-11-24T12:00:00Z",
            &days("2026-11-21", &[(GB, Some(GB)); 3]),
        );
        assert_eq!(logs_of(&input).unwrap().recent, recent, "{since}");
    }
}

#[test]
fn full_on_range_runs_the_forecast_at_low_and_high_growth() {
    // 10 GB to the limit: 12 days at the high growth, 18 at the low.
    let f = forecast(&three_days(), &WhatIf::default()).unwrap();
    assert_eq!(
        f.full_on_range,
        [Some(day("2026-12-06")), Some(day("2026-12-12"))]
    );
    // The low growth never reaches the limit once retention caps it.
    let mut capped = three_days();
    capped.projects[0].1 = 15;
    let f = forecast(&capped, &WhatIf::default()).unwrap();
    assert_eq!(f.full_on_range, [Some(day("2026-12-06")), None]);
}

#[test]
fn a_blended_bound_uses_the_high_growth() {
    let f = forecast(&three_days(), &WhatIf::default()).unwrap();
    assert_eq!(
        (f.growth, f.days_left, f.full_on),
        (873_772_000, Some(12), Some(day("2026-12-06")))
    );
    // Measured days bound on their median.
    let three = days(
        "2026-11-21",
        &[(GB, Some(GB)), (GB, Some(2 * GB)), (GB, Some(3 * GB))],
    );
    let mut measured = server("2026-11-21T00:00:00Z", "2026-11-24T12:00:00Z", &three);
    measured.projects[0].1 = 3;
    let f = forecast(&measured, &WhatIf::default()).unwrap();
    assert_eq!((f.logs.unwrap().basis, f.growth), (Basis::Measured, 2 * GB));
}

#[test]
fn the_high_growth_rounds_up_and_the_rest_down() {
    let three = [
        (GB + 1, Some(600 * MB)),
        (GB + 1, Some(650 * MB)),
        (GB + 1, Some(700 * MB)),
    ];
    let input = server(
        "2026-11-21T00:00:00Z",
        "2026-11-24T12:00:00Z",
        &days("2026-11-21", &three),
    );
    let l = logs_of(&input).unwrap();
    assert_eq!(
        (l.ratio, l.growth),
        (
            Some(Band {
                low: 579_199,
                mid: 713_428,
                high: 873_771,
            }),
            Spread {
                low: 579_199_000,
                mid: 713_428_000,
                high: 873_771_001,
            }
        )
    );
}
