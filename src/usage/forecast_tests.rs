use super::*;

const GB: u64 = 1_000_000_000;
const MB: u64 = 1_000_000;

fn day(s: &str) -> Date {
    s.parse().unwrap()
}

/// `sizes` as the closed days that end yesterday, 2026-11-23.
fn closed(sizes: &[u64]) -> Vec<(Date, u64)> {
    let yesterday = day("2026-11-23");
    let n = i64::try_from(sizes.len()).unwrap();
    (0..n)
        .zip(sizes)
        .map(|(i, b)| (yesterday.checked_sub((n - 1 - i).days()).unwrap(), *b))
        .collect()
}

fn projects(kept: &[(&str, u32)]) -> Vec<(String, u32)> {
    kept.iter().map(|(s, n)| ((*s).to_owned(), *n)).collect()
}

/// 8 GB of logs a day on a 500 GB disk with 300 GB used, of which Davy
/// takes 50. Both projects keep logs 30 days.
fn filling() -> Input {
    Input {
        today: day("2026-11-24"),
        closed: closed(&[9, 7, 8, 8, 10, 6, 8].map(|n| n * GB)),
        davy: 50 * GB,
        fixed: 10 * GB,
        used: 300 * GB,
        total: 500 * GB,
        projects: projects(&[("api", 30), ("shop", 30)]),
        default_days: 30,
    }
}

#[test]
fn a_disk_that_fills_says_when() {
    let f = forecast(&filling(), &WhatIf::default()).unwrap();
    assert_eq!(
        f,
        Forecast {
            growth: 8 * GB,
            longest_days: 30,
            // 8 GB for 30 days, and 10 GB of SQLite and uploads.
            steady_state: 250 * GB,
            headroom: 50 * GB,
            // 150 GB to the limit at 8 GB a day.
            days_left: Some(19),
            full_on: Some(day("2026-12-13")),
        }
    );
}

#[test]
fn retention_that_caps_growth_first_means_the_disk_never_fills() {
    let input = Input {
        projects: projects(&[("api", 7), ("shop", 7)]),
        ..filling()
    };
    let f = forecast(&input, &WhatIf::default()).unwrap();
    assert_eq!(
        (f.steady_state, f.days_left, f.full_on),
        (66 * GB, None, None)
    );
}

#[test]
fn a_short_retention_with_most_of_the_logs_cannot_hide_a_filling_disk() {
    // api sends 9.5 of 10 GB a day and keeps 1 day, so every day before
    // yesterday has lost its logs and shows 500 MB.
    let input = Input {
        closed: closed(&[500 * MB, 500 * MB, 500 * MB, 500 * MB, 500 * MB, 10 * GB]),
        projects: projects(&[("api", 1), ("shop", 30)]),
        ..filling()
    };
    let f = forecast(&input, &WhatIf::default()).unwrap();
    // 10 GB for 30 days is past the limit: 150 GB at 10 GB a day.
    assert_eq!(
        (
            f.growth,
            f.longest_days,
            f.steady_state,
            f.days_left,
            f.full_on
        ),
        (10 * GB, 30, 310 * GB, Some(15), Some(day("2026-12-09")))
    );
}

#[test]
fn only_closed_days_within_the_shortest_retention_count() {
    let input = Input {
        closed: closed(&[1, 1, 1, 1, 10, 6, 8].map(|n| n * GB)),
        projects: projects(&[("api", 3), ("shop", 30)]),
        ..filling()
    };
    let f = forecast(&input, &WhatIf::default()).unwrap();
    // 2026-11-21 to 2026-11-23: 10, 6 and 8 GB.
    assert_eq!(f.growth, 8 * GB);
}

#[test]
fn too_few_closed_days_no_retention_has_cut_give_no_forecast() {
    for (sizes, kept, first, need) in [
        (vec![], 30, "2026-10-25", 2),
        (vec![5 * GB], 30, "2026-10-25", 2),
        (vec![], 1, "2026-11-23", 1),
    ] {
        let input = Input {
            closed: closed(&sizes),
            projects: projects(&[("api", kept)]),
            ..filling()
        };
        assert_eq!(
            forecast(&input, &WhatIf::default()),
            Err(format!(
                "closed log days since {first}, the first day no retention has cut: the disk has {}, a forecast needs {need}",
                sizes.len()
            ))
        );
    }
}

#[test]
fn a_shorter_retention_lowers_the_forecast_only_when_it_lowers_the_longest() {
    let all = WhatIf {
        logs_days: Some(7),
        ..WhatIf::default()
    };
    let f = forecast(&filling(), &all).unwrap();
    assert_eq!(
        (f.longest_days, f.steady_state, f.days_left),
        (7, 66 * GB, None)
    );
    let shop = WhatIf {
        project: Some("shop".into()),
        logs_days: Some(4),
        ..WhatIf::default()
    };
    let f = forecast(&filling(), &shop).unwrap();
    assert_eq!(
        (f.longest_days, f.steady_state, f.days_left),
        (30, 250 * GB, Some(19))
    );
}

#[test]
fn a_total_daily_volume_forecasts_without_history() {
    let input = Input {
        closed: vec![],
        davy: 10 * GB,
        used: 440 * GB,
        projects: projects(&[("api", 30)]),
        ..filling()
    };
    let what_if = WhatIf {
        logs_bytes_per_day: Some(2 * GB),
        ..WhatIf::default()
    };
    let f = forecast(&input, &what_if).unwrap();
    assert_eq!(
        (f.growth, f.steady_state, f.days_left, f.full_on),
        (2 * GB, 70 * GB, Some(5), Some(day("2026-11-29")))
    );
}

#[test]
fn a_daily_volume_too_large_to_multiply_fills_the_disk_in_a_day() {
    let what_if = WhatIf {
        logs_bytes_per_day: Some(u64::MAX),
        ..WhatIf::default()
    };
    let f = forecast(&filling(), &what_if).unwrap();
    assert_eq!(
        (f.steady_state, f.days_left, f.full_on),
        (u64::MAX, Some(1), Some(day("2026-11-25")))
    );
}

#[test]
fn a_disk_past_the_limit_is_full_today() {
    let input = Input {
        used: 460 * GB,
        ..filling()
    };
    let f = forecast(&input, &WhatIf::default()).unwrap();
    assert_eq!((f.days_left, f.full_on), (Some(0), Some(day("2026-11-24"))));
}

#[test]
fn a_what_if_must_name_a_project_that_exists_keep_logs_and_not_split_bytes() {
    let unknown = WhatIf {
        project: Some("nope".into()),
        logs_days: Some(7),
        ..WhatIf::default()
    };
    assert_eq!(
        forecast(&filling(), &unknown),
        Err(crate::retention::unknown("nope"))
    );
    let zero = WhatIf {
        logs_days: Some(0),
        ..WhatIf::default()
    };
    assert_eq!(
        forecast(&filling(), &zero),
        Err("logs_days must be 1 to 36500".into())
    );
    let one = WhatIf {
        project: Some("shop".into()),
        logs_bytes_per_day: Some(GB),
        ..WhatIf::default()
    };
    assert_eq!(
        forecast(&filling(), &one),
        Err("logs_bytes_per_day is for all projects together: logs of every project share one index per day, so Davy cannot tell one project's bytes on disk".into())
    );
}

#[test]
fn the_median_of_an_even_count_is_the_mean_of_the_middle_two() {
    assert_eq!(median(&[4, 1, 3, 2]), Some(2));
    assert_eq!(median(&[5, 1, 9]), Some(5));
    assert_eq!(median(&[u64::MAX, u64::MAX]), Some(u64::MAX));
    assert_eq!(median(&[]), None);
}
