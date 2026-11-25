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
            shortest_days: 30,
            // 8 GB for 30 closed days and today, and 10 GB of SQLite and
            // uploads.
            steady_state: 258 * GB,
            headroom: 50 * GB,
            // 150 GB to the limit at 8 GB a day.
            days_left: Some(19),
            full_on: Some(day("2026-12-13")),
            unseen: None,
        }
    );
    assert!(!f.worst_case());
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
        (74 * GB, None, None)
    );
}

#[test]
fn a_retention_of_n_days_holds_n_closed_days_and_today() {
    // Every project keeps logs 1 day and adds 45 GB a day. This morning
    // the disk holds yesterday and 5 GB of SQLite; by tonight today's 45 GB
    // sit next to yesterday's and pass the 90 GB limit.
    let input = Input {
        closed: closed(&[45 * GB]),
        davy: 50 * GB,
        fixed: 5 * GB,
        used: 50 * GB,
        total: 100 * GB,
        projects: projects(&[("api", 1), ("shop", 1)]),
        ..filling()
    };
    let f = forecast(&input, &WhatIf::default()).unwrap();
    assert_eq!(
        (f.steady_state, f.days_left, f.full_on),
        (95 * GB, Some(1), Some(day("2026-11-25")))
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
    // 10 GB for 31 days is past the limit: 150 GB at 10 GB a day.
    assert_eq!(
        (
            f.growth,
            f.longest_days,
            f.steady_state,
            f.days_left,
            f.full_on
        ),
        (10 * GB, 30, 320 * GB, Some(15), Some(day("2026-12-09")))
    );
    assert_eq!((f.shortest_days, f.worst_case()), (1, true));
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
            Err(NoForecast::History(format!(
                "closed log days since {first}, the first day no retention has cut: the disk has {}, a forecast needs {need}",
                sizes.len()
            )))
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
        (7, 74 * GB, None)
    );
    let shop = WhatIf {
        project: Some("shop".into()),
        logs_days: Some(4),
        ..WhatIf::default()
    };
    let f = forecast(&filling(), &shop).unwrap();
    assert_eq!(
        (f.longest_days, f.steady_state, f.days_left),
        (30, 258 * GB, Some(19))
    );
    assert_eq!(
        f.unseen.as_deref(),
        Some(
            "the forecast cannot show this what-if: it takes the logs of every project as kept 30 days, as long as api keeps them, and logs of all projects share one index per day, so Davy cannot tell how much of the growth is shop's. The worst case stays as it is, though shop's own logs would take less disk with a shorter retention and more with a longer one"
        )
    );
    let same = WhatIf {
        logs_days: Some(30),
        ..shop
    };
    assert_eq!(forecast(&filling(), &same).unwrap().unseen, None);
}

/// api sends 9.9 GB of logs a day and keeps 30 days, audit sends 0.1 GB
/// and keeps 365. The disk holds 340 GB of 1 TB and has for months.
fn mixed() -> Input {
    Input {
        closed: closed(&[10 * GB; 7]),
        davy: 300 * GB,
        fixed: 3 * GB,
        used: 340 * GB,
        total: 1000 * GB,
        projects: projects(&[("api", 30), ("audit", 365)]),
        ..filling()
    }
}

#[test]
fn mixed_retentions_give_a_worst_case() {
    let f = forecast(&mixed(), &WhatIf::default()).unwrap();
    // 560 GB to the limit at 10 GB a day, were every log kept 365 days.
    assert_eq!(
        (
            f.shortest_days,
            f.longest_days,
            f.days_left,
            f.full_on,
            f.worst_case()
        ),
        (30, 365, Some(56), Some(day("2027-01-19")), true)
    );
}

#[test]
fn a_what_if_the_worst_case_cannot_show_says_why() {
    let api = WhatIf {
        project: Some("api".into()),
        logs_days: Some(7),
        ..WhatIf::default()
    };
    let f = forecast(&mixed(), &api).unwrap();
    assert_eq!((f.days_left, f.worst_case()), (Some(56), true));
    assert_eq!(
        f.unseen.as_deref(),
        Some(
            "the forecast cannot show this what-if: it takes the logs of every project as kept 365 days, as long as audit keeps them, and logs of all projects share one index per day, so Davy cannot tell how much of the growth is api's. The worst case stays as it is, though api's own logs would take less disk with a shorter retention and more with a longer one"
        )
    );
    // Cutting the project that holds the longest retention moves it, and
    // with one retention left the forecast is no worst case.
    let audit = WhatIf {
        project: Some("audit".into()),
        logs_days: Some(30),
        ..WhatIf::default()
    };
    let f = forecast(&mixed(), &audit).unwrap();
    assert_eq!(
        (f.longest_days, f.days_left, f.worst_case(), f.unseen),
        (30, None, false, None)
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
        (2 * GB, 72 * GB, Some(5), Some(day("2026-11-29")))
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
    let check = |what_if: WhatIf| what_if.check(["api", "shop"].into_iter());
    let unknown = WhatIf {
        project: Some("nope".into()),
        logs_days: Some(7),
        ..WhatIf::default()
    };
    assert_eq!(check(unknown), Err(crate::retention::unknown("nope")));
    let zero = WhatIf {
        logs_days: Some(0),
        ..WhatIf::default()
    };
    assert_eq!(check(zero), Err("logs_days must be 1 to 36500".into()));
    let one = WhatIf {
        project: Some("shop".into()),
        logs_bytes_per_day: Some(GB),
        ..WhatIf::default()
    };
    assert_eq!(
        check(one),
        Err("logs_bytes_per_day is for all projects together: logs of every project share one index per day, so Davy cannot tell one project's bytes on disk".into())
    );
    let nothing = WhatIf {
        project: Some("shop".into()),
        ..WhatIf::default()
    };
    assert_eq!(
        check(nothing),
        Err("a what-if for project shop needs logs_days, the days of logs it would keep".into())
    );
}

#[test]
fn the_median_of_an_even_count_is_the_mean_of_the_middle_two() {
    assert_eq!(median(&[4, 1, 3, 2]), Some(2));
    assert_eq!(median(&[5, 1, 9]), Some(5));
    assert_eq!(median(&[u64::MAX, u64::MAX]), Some(u64::MAX));
    assert_eq!(median(&[]), None);
}

#[test]
fn a_disk_that_reaches_the_limit_exactly_at_steady_state_fills() {
    // 10 GB a day kept 14 days and today: 160 GB, 110 GB more than Davy
    // takes now. The limit is 450 GB.
    let at = |used| {
        let input = Input {
            used,
            projects: projects(&[("api", 14)]),
            ..filling()
        };
        let what_if = WhatIf {
            logs_bytes_per_day: Some(10 * GB),
            ..WhatIf::default()
        };
        forecast(&input, &what_if).unwrap().days_left
    };
    assert_eq!(
        (at(339 * GB), at(340 * GB), at(450 * GB)),
        (None, Some(11), Some(0))
    );
}

#[test]
fn no_growth_never_fills_the_disk() {
    let what_if = WhatIf {
        logs_bytes_per_day: Some(0),
        ..WhatIf::default()
    };
    let f = forecast(&filling(), &what_if).unwrap();
    assert_eq!((f.steady_state, f.days_left), (10 * GB, None));
}

#[test]
fn a_server_without_projects_takes_its_default_retention() {
    let input = Input {
        projects: Vec::new(),
        default_days: 30,
        ..filling()
    };
    let f = forecast(&input, &WhatIf::default()).unwrap();
    assert_eq!(
        (f.longest_days, f.shortest_days, f.days_left, f.worst_case()),
        (30, 30, Some(19), false)
    );
}

#[test]
fn davy_already_past_its_steady_state_never_fills_the_disk() {
    // 8 GB a day kept 30 days and today is 248 GB of logs and 10 GB of
    // SQLite and uploads: 258 GB, less than the 270 GB Davy takes now, as
    // after a retention was shortened.
    let input = Input {
        davy: 270 * GB,
        ..filling()
    };
    let f = forecast(&input, &WhatIf::default()).unwrap();
    assert_eq!(
        (f.growth, f.steady_state, f.days_left, f.full_on),
        (8 * GB, 258 * GB, None, None)
    );
}
