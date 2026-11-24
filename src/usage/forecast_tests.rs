use super::*;

const GB: u64 = 1_000_000_000;

fn day(s: &str) -> Date {
    s.parse().unwrap()
}

fn project(slug: &str, log_bytes: u64, logs_days: u32) -> Project {
    Project {
        slug: slug.into(),
        log_bytes,
        logs_days,
    }
}

/// 8 GB of logs a day, 6 for api and 2 for shop, on a 500 GB disk with
/// 300 GB used, of which Davy takes 50.
fn filling() -> Input {
    Input {
        today: day("2026-11-24"),
        closed: [9, 7, 8, 8, 10, 6, 8].map(|n| n * GB).to_vec(),
        davy: 50 * GB,
        fixed: 10 * GB,
        used: 300 * GB,
        total: 500 * GB,
        projects: vec![project("api", 3_000, 30), project("shop", 1_000, 30)],
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
            per_project: vec![
                Growth {
                    project: Some("api".into()),
                    bytes_per_day: 6 * GB,
                    logs_days: 30,
                },
                Growth {
                    project: Some("shop".into()),
                    bytes_per_day: 2 * GB,
                    logs_days: 30,
                },
            ],
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
    let mut input = filling();
    input.projects = vec![project("api", 3_000, 7), project("shop", 1_000, 7)];
    let f = forecast(&input, &WhatIf::default()).unwrap();
    assert_eq!(
        (f.steady_state, f.days_left, f.full_on),
        (66 * GB, None, None)
    );
}

#[test]
fn fewer_than_two_closed_days_give_no_forecast() {
    for closed in [vec![], vec![5 * GB]] {
        let input = Input {
            closed: closed.clone(),
            ..filling()
        };
        assert_eq!(
            forecast(&input, &WhatIf::default()),
            Err(format!(
                "a forecast needs at least 2 closed log days, and the disk has {}",
                closed.len()
            ))
        );
    }
}

#[test]
fn a_shorter_retention_for_one_project_can_stop_the_disk_filling() {
    let what_if = WhatIf {
        project: Some("shop".into()),
        logs_days: Some(4),
        logs_bytes_per_day: None,
    };
    let f = forecast(&filling(), &what_if).unwrap();
    // 6 GB for 30 days, 2 GB for 4 days and 10 GB: 198 GB, and 148 GB more
    // leaves the disk 2 GB short of the limit.
    assert_eq!((f.steady_state, f.days_left), (198 * GB, None));
    assert_eq!(f.per_project[1].logs_days, 4);
}

#[test]
fn a_total_daily_volume_forecasts_without_history() {
    let input = Input {
        closed: vec![],
        davy: 10 * GB,
        used: 440 * GB,
        projects: vec![project("api", 0, 30)],
        ..filling()
    };
    let what_if = WhatIf {
        logs_bytes_per_day: Some(2 * GB),
        ..WhatIf::default()
    };
    let f = forecast(&input, &what_if).unwrap();
    assert_eq!(
        f.per_project,
        vec![Growth {
            project: None,
            bytes_per_day: 2 * GB,
            logs_days: 30,
        }]
    );
    assert_eq!(
        (f.steady_state, f.days_left, f.full_on),
        (70 * GB, Some(5), Some(day("2026-11-29")))
    );
}

#[test]
fn one_projects_daily_volume_replaces_its_share() {
    let what_if = WhatIf {
        project: Some("shop".into()),
        logs_bytes_per_day: Some(GB),
        ..WhatIf::default()
    };
    let f = forecast(&filling(), &what_if).unwrap();
    assert_eq!((f.growth, f.steady_state), (7 * GB, 220 * GB));
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
fn a_what_if_must_name_a_project_that_exists_and_keep_logs() {
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
}

#[test]
fn the_median_of_an_even_count_is_the_mean_of_the_middle_two() {
    assert_eq!(median(&[4, 1, 3, 2]), Some(2));
    assert_eq!(median(&[5, 1, 9]), Some(5));
    assert_eq!(median(&[u64::MAX, u64::MAX]), Some(u64::MAX));
    assert_eq!(median(&[]), None);
}
