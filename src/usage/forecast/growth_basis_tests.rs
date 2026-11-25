use super::tests::{GB, HOUR, MB, day, days, logs_of, server, three_days};
use super::*;
use crate::usage::forecast::{WhatIf, forecast};

/// A server installed at 18:00 on 2026-11-20: the first day holds 250 MB
/// raw and 175 MB on disk, each whole day 1 GB raw and 700 MB on disk.
fn installed_at_six(now: &str, whole_days: usize) -> Input {
    let mut values = vec![(250 * MB, Some(175 * MB))];
    values.extend(std::iter::repeat_n((GB, Some(700 * MB)), whole_days));
    server("2026-11-20T18:00:00Z", now, &days("2026-11-20", &values))
}

#[test]
fn a_new_server_blends_from_its_first_whole_day() {
    for (now, whole_days, basis, growth, confidence) in [
        (
            "2026-11-22T12:00:00Z",
            1,
            Basis::Blended,
            Spread {
                low: 583_085_000,
                mid: 752_285_000,
                high: 960_658_000,
            },
            Confidence::Low,
        ),
        (
            "2026-11-26T12:00:00Z",
            5,
            Basis::Blended,
            Spread {
                low: 661_028_000,
                mid: 717_428_000,
                high: 786_886_000,
            },
            Confidence::Low,
        ),
        (
            "2026-11-28T12:00:00Z",
            7,
            Basis::Measured,
            Spread::flat(700 * MB),
            Confidence::High,
        ),
    ] {
        let l = logs_of(&installed_at_six(now, whole_days)).unwrap();
        assert_eq!(
            (l.basis, l.own_days, l.growth, l.confidence(false)),
            (basis, whole_days, growth, confidence),
            "{now}"
        );
    }
}

#[test]
fn a_new_server_keeping_logs_1_day_forecasts_from_its_hours() {
    // Installed at 23:00: the closed day holds one hour of logs.
    let two = [(40 * MB, Some(30 * MB)), (500 * MB, None)];
    let mut input = server(
        "2026-11-23T23:00:00Z",
        "2026-11-24T12:00:00Z",
        &days("2026-11-23", &two),
    );
    input.projects[0].1 = 1;
    let l = logs_of(&input).unwrap();
    // 540 MB in 13 hours.
    assert_eq!(
        (l.basis, l.possible_days, l.counted_ns, l.raw, l.growth),
        (
            Basis::FirstDay,
            1,
            Some(13 * HOUR),
            Some(Spread {
                low: 332_307_692,
                mid: 996_923_076,
                high: 2_990_769_230,
            }),
            Spread {
                low: 187_921_171,
                mid: 759_291_016,
                high: 3_003_663_940,
            }
        )
    );
}

#[test]
fn a_partial_closed_day_waits_for_an_hour_counted() {
    let two = [(20 * MB, Some(15 * MB)), (5 * MB, None)];
    let mut input = server(
        "2026-11-23T23:30:00Z",
        "2026-11-24T00:20:00Z",
        &days("2026-11-23", &two),
    );
    input.projects[0].1 = 1;
    assert_eq!(
        forecast(&input, &WhatIf::default()),
        Err(NoForecast::History(
            "Davy has counted raw log bytes for 50 minutes; a first forecast needs an hour of them, or closed log days on disk".to_owned()
        ))
    );
}

#[test]
fn possible_reads_the_retention_of_projects_with_raw_bytes_then_the_default() {
    let mut quiet_short = three_days();
    quiet_short.projects.push(("audit".to_owned(), 3));
    let mut no_projects = three_days();
    no_projects.projects.clear();
    no_projects.default_days = 3;
    for (name, input, possible, basis) in [
        ("audit has no raw bytes", quiet_short, 7, Basis::Blended),
        ("no projects", no_projects, 3, Basis::Measured),
    ] {
        let l = logs_of(&input).unwrap();
        assert_eq!((l.possible_days, l.basis), (possible, basis), "{name}");
    }
}

#[test]
fn possible_reads_raw_bytes_of_the_newest_30_days_only() {
    // audit keeps logs 3 days and sent its last raw bytes on one day:
    // 30 days before today it still counts, 31 days before it does not.
    for (on, possible) in [("2026-10-25", 3), ("2026-10-24", 7)] {
        let mut input = three_days();
        input.projects.push(("audit".to_owned(), 3));
        input.raw.insert(
            0,
            RawDay {
                day: day(on),
                projects: vec![("audit".to_owned(), GB, false)],
            },
        );
        assert_eq!(logs_of(&input).unwrap().possible_days, possible, "{on}");
    }
}

#[test]
fn only_the_newest_possible_days_count_for_the_ratio() {
    // Right after a retention was shortened to 3, 5 days still qualify.
    let five: Vec<(u64, Option<u64>)> = (1..=5).map(|n| (10 * GB, Some(n * GB))).collect();
    let mut input = server(
        "2026-11-01T00:00:00Z",
        "2026-11-24T12:00:00Z",
        &days("2026-11-19", &five),
    );
    input.projects[0].1 = 3;
    let l = logs_of(&input).unwrap();
    assert_eq!(
        (l.basis, l.own_days, l.growth),
        (
            Basis::Measured,
            3,
            Spread {
                low: 3 * GB,
                mid: 4 * GB,
                high: 5 * GB,
            }
        )
    );
}

#[test]
fn a_blend_takes_the_low_and_high_raw_bytes_to_its_ends() {
    let three = [
        (GB, Some(600 * MB)),
        (2 * GB, Some(1_300 * MB)),
        (4 * GB, Some(2_800 * MB)),
    ];
    let input = server(
        "2026-11-21T00:00:00Z",
        "2026-11-24T12:00:00Z",
        &days("2026-11-21", &three),
    );
    let l = logs_of(&input).unwrap();
    // The middle raw bytes are the mean of 7 GB in 3 days, above the
    // median of 2 GB.
    assert_eq!(
        (l.basis, l.raw, l.growth),
        (
            Basis::Blended,
            Some(Spread {
                low: GB,
                mid: 2_333_333_333,
                high: 4 * GB,
            }),
            Spread {
                low: 579_200_000,
                mid: 1_664_665_333,
                high: 3_495_088_000,
            }
        )
    );
}

#[test]
fn a_whole_day_without_a_size_on_disk_does_not_count_for_the_ratio() {
    let three = [(GB, Some(600 * MB)), (GB, None), (GB, Some(700 * MB))];
    let input = server(
        "2026-11-21T00:00:00Z",
        "2026-11-24T12:00:00Z",
        &days("2026-11-21", &three),
    );
    let l = logs_of(&input).unwrap();
    assert_eq!((l.own_days, l.raw_days, l.own_ratio), (2, 3, Some(650_000)));
}

/// api keeps logs 30 days and shop 3. Retention cut shop from the days
/// before 2026-11-21, which hold api's 400 MB on disk only.
#[test]
fn mixed_retentions_measure_the_days_the_short_one_kept_and_bound_the_worst_case() {
    let disks = [400, 400, 400, 400, 600, 700, 900];
    let values: Vec<(u64, Option<u64>)> = disks.iter().map(|n| (GB, Some(n * MB))).collect();
    let list = days("2026-11-17", &values);
    let mut input = server("2026-11-01T00:00:00Z", "2026-11-24T12:00:00Z", &list);
    input.projects.push(("shop".to_owned(), 3));
    input.raw = list
        .iter()
        .map(|(d, _, _)| RawDay {
            day: *d,
            projects: vec![
                ("api".to_owned(), 400 * MB, false),
                ("shop".to_owned(), 600 * MB, *d < day("2026-11-21")),
            ],
        })
        .collect();
    let f = forecast(&input, &WhatIf::default()).unwrap();
    let l = f.logs.clone().unwrap();
    assert_eq!(
        (l.basis, l.possible_days, l.own_days, l.growth),
        (
            Basis::Measured,
            3,
            3,
            Spread {
                low: 600 * MB,
                mid: 700 * MB,
                high: 900 * MB,
            }
        )
    );
    // 10 GB to the limit: 15 days at 700 MB, 12 at 900 MB, 17 at 600 MB,
    // each as if every log were kept 30 days.
    assert_eq!(
        (
            f.worst_case(),
            f.growth,
            f.days_left,
            f.full_on,
            f.full_on_range
        ),
        (
            true,
            700 * MB,
            Some(15),
            Some(day("2026-12-09")),
            [Some(day("2026-12-06")), Some(day("2026-12-11"))]
        )
    );
}

#[test]
fn the_answer_says_why_the_confidence_is_low() {
    let recent = logs_of(&three_days()).unwrap();
    let blend = "3 of 7 closed log days measured on this server in the last 30; the rest of the ratio comes from the footprint benchmark";
    let old = Logs {
        recent: false,
        ..recent.clone()
    };
    for (l, partial, why) in [
        (
            &recent,
            false,
            format!(
                "{blend}; confidence is low while Davy has counted raw log bytes for under 7 whole days"
            ),
        ),
        (
            &old,
            true,
            format!("{blend}; confidence is low as the disk walk left something out"),
        ),
        (&old, false, blend.to_owned()),
    ] {
        assert_eq!(l.explain(partial), why, "{partial}");
    }
}

#[test]
fn a_line_dated_before_the_install_does_not_make_a_new_server_upgraded() {
    // The server of `installed_at_six` stored one line dated 2026-11-19,
    // 4 KB raw and 700 KB on disk, as a shipper replaying a file does. It
    // wrote a `log_days` row for that day, which an older version did not.
    let with_old_line = |now: &str, whole_days: usize| {
        let mut values = vec![(4_000, Some(700_000)), (250 * MB, Some(175 * MB))];
        values.extend(std::iter::repeat_n((GB, Some(700 * MB)), whole_days));
        server("2026-11-20T18:00:00Z", now, &days("2026-11-19", &values))
    };
    let first = logs_of(&with_old_line("2026-11-21T12:00:00Z", 0)).unwrap();
    assert_eq!(first.basis, Basis::FirstDay);
    for (now, whole_days) in [("2026-11-22T12:00:00Z", 1), ("2026-11-23T12:00:00Z", 2)] {
        let l = logs_of(&with_old_line(now, whole_days)).unwrap();
        let without = logs_of(&installed_at_six(now, whole_days)).unwrap();
        assert_eq!(
            (l.basis, l.own_days, l.growth, l.why.as_str()),
            (
                Basis::Blended,
                whole_days,
                without.growth,
                without.why.as_str()
            ),
            "{now}"
        );
    }
    assert_eq!(
        logs_of(&with_old_line("2026-11-22T12:00:00Z", 1))
            .unwrap()
            .growth,
        Spread {
            low: 583_085_000,
            mid: 752_285_000,
            high: 960_658_000,
        }
    );
}

/// Upgraded at 09:00 on 2026-11-03, a Tuesday, keeping logs 90 days: on
/// Mondays only, 5 MB raw and 4 MB on disk.
fn upgraded_mondays(now: &str) -> Input {
    let from = day("2026-10-26");
    let mondays: Vec<(Date, u64, Option<u64>)> = (0..8)
        .map(|w| {
            (
                from.checked_add((7 * w).days()).unwrap(),
                5 * MB,
                Some(4 * MB),
            )
        })
        .collect();
    let mut input = server("2026-11-03T09:00:00Z", now, &mondays);
    input.projects[0].1 = 90;
    // The older version wrote no `log_days` rows.
    input.raw.retain(|r| r.day >= day("2026-11-03"));
    input
}

#[test]
fn an_upgraded_server_with_small_days_says_they_are_small() {
    let small = "no whole closed day could measure how logs pack, as each had under 10 MB of raw logs, a project that retention cut, or no size on disk";
    let since = "Davy has counted raw log bytes only since 2026-11-03, too few whole days to measure how they pack";
    for (now, sizes, why) in [
        // 6 whole days, one fewer than `possible`.
        ("2026-11-10T12:00:00Z", 3, since),
        // 7 whole days, none with 10 MB.
        ("2026-11-11T12:00:00Z", 3, small),
        ("2026-12-15T12:00:00Z", 7, small),
    ] {
        let l = logs_of(&upgraded_mondays(now)).unwrap();
        assert_eq!(
            (l.basis, l.growth, l.why),
            (
                Basis::MeasuredDisk,
                Spread::flat(4 * MB),
                format!("the growth is the median size on disk of {sizes} closed log days: {why}")
            ),
            "{now}"
        );
    }
}

#[test]
fn an_upgraded_server_with_one_measured_day_still_says_too_few() {
    // The newest Monday holds 20 MB raw: 1 of 7 days measured.
    let mut input = upgraded_mondays("2026-12-15T12:00:00Z");
    input.raw.last_mut().unwrap().projects[0].1 = 20 * MB;
    let l = logs_of(&input).unwrap();
    assert_eq!(
        (l.basis, l.own_days, l.growth.mid, l.why),
        (
            Basis::MeasuredDisk,
            1,
            4 * MB,
            "the growth is the median size on disk of 7 closed log days: Davy has counted raw log bytes only since 2026-11-03, too few whole days to measure how they pack".to_owned()
        )
    );
}

#[test]
fn only_the_closed_days_of_the_direct_forecast_tell_an_upgraded_server() {
    // Upgraded at 09:00 on 2026-11-10. The old version wrote 2026-11-01 to
    // 2026-11-09, which the disk still holds; the newest 7 closed days
    // are all counted. The server was quiet, 5 MB raw a day, and got busy
    // 3 days ago.
    let mut values = vec![(GB, Some(700 * MB)); 9];
    values.extend([(5 * MB, Some(4 * MB)); 11]);
    values.extend([(GB, Some(700 * MB)); 3]);
    let mut input = server(
        "2026-11-10T09:00:00Z",
        "2026-11-24T12:00:00Z",
        &days("2026-11-01", &values),
    );
    input.raw.retain(|r| r.day >= day("2026-11-10"));
    let l = logs_of(&input).unwrap();
    assert_eq!((l.basis, l.own_days), (Basis::Blended, 3));
}

#[test]
fn the_day_counting_started_never_makes_a_server_upgraded() {
    // Upgraded at 15:00 on 2026-11-22, keeping logs 2 days. The old
    // version wrote all of that day, so it has no `log_days` row.
    let two = days("2026-11-22", &[(10 * GB, Some(8 * GB)); 2]);
    let mut input = server("2026-11-22T15:00:00Z", "2026-11-24T12:00:00Z", &two);
    input.projects[0].1 = 2;
    input.raw.retain(|r| r.day > day("2026-11-22"));
    let l = logs_of(&input).unwrap();
    assert_eq!(
        (l.basis, l.own_days, l.possible_days),
        (Basis::Blended, 1, 2)
    );
}
