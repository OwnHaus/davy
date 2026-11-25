use super::*;
use crate::{
    logs::store::log_days,
    store,
    usage::{
        footprint::{Band, FIXTURE, PRIOR},
        forecast::{Basis, ErrorBasis, Errors, Estimate, Fill, Logs, Spread},
        stored,
    },
};

const GB: u64 = 1_000_000_000;
const MB: u64 = 1_000_000;

fn day(s: &str) -> Date {
    s.parse().unwrap()
}

fn at(s: &str) -> i64 {
    time::nanos(s.parse().unwrap()).unwrap()
}

/// The blend of 3 own days of 1 GB raw with the fixture prior.
fn blended() -> Logs {
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
        raw_days: 5,
        counted_ns: None,
        recent: false,
        ratio: Some(Band {
            low: 579_200,
            mid: 713_428,
            high: 873_772,
        }),
        own_ratio: Some(650_000),
        prior: FIXTURE,
        why: "3 of 7 closed log days measured on this server in the last 30; the rest of the ratio comes from the footprint benchmark".to_owned(),
    }
}

fn report(logs: Option<Logs>, range: [Option<Date>; 2]) -> Report {
    Report {
        since: day("2026-11-24"),
        days: 1.0,
        disk: Ok(Disk {
            total: 500 * GB,
            free: 60 * GB,
            ..Disk::default()
        }),
        forecast: Ok(Forecast {
            growth: 873_772_000,
            longest_days: 30,
            shortest_days: 30,
            steady_state: 28_086_932_000,
            headroom: 50 * GB,
            days_left: Some(12),
            full_on: Some(day("2026-12-06")),
            errors: None,
            beyond_10_years: false,
            estimate: None,
            unseen: None,
            logs,
            full_on_range: range,
        }),
        what_if: WhatIf::default(),
        projects: Vec::new(),
        default_log_days: None,
    }
}

#[test]
fn the_basis_states_its_confidence_and_range_in_json_and_text() {
    let r = report(
        Some(blended()),
        [Some(day("2026-12-06")), Some(day("2026-12-12"))],
    );
    assert_eq!(
        r.json(true)["basis"],
        json!({
            "confidence": "medium",
            "errors": null,
            "full_on_range": ["2026-12-06", "2026-12-12"],
            "logs": {
                "basis": "blended",
                "growth_per_day": {"low": "579.2 MB", "mid": "713.4 MB", "high": "873.8 MB"},
                "own_days": 3,
                "possible_days": 7,
                "raw_per_day": "1.0 GB",
                "raw_days": 5,
                "counted_hours": null,
                "ratio": 0.7134,
                "ratio_range": [0.5792, 0.8738],
                "own_ratio": 0.65,
                "footprint_ratio": 0.761,
                "footprint": "fixture",
            },
            "why": "3 of 7 closed log days measured on this server in the last 30; the rest of the ratio comes from the footprint benchmark",
        })
    );
    assert_eq!(r.json(false)["basis"]["logs"]["raw_per_day"], json!(GB));
    let mut out = Vec::new();
    r.text(&mut out).unwrap();
    let text = String::from_utf8(out).unwrap();
    for line in [
        "basis     blended, medium confidence: 3 of 7 closed log days measured on this server in the last 30; the rest of the ratio comes from the footprint benchmark\n",
        "range     full between 2026-12-06 and 2026-12-12, at 873.8 MB to 579.2 MB of logs a day\n",
    ] {
        assert!(text.contains(line), "{line} in {text}");
    }
}

/// Two projects' errors on 3 measured days, and an estimate a week later
/// than the bound.
fn with_errors(r: &mut Report, steady: u64, fill: Fill) {
    if let Ok(f) = &mut r.forecast {
        f.errors = Some(Errors {
            projects: 2,
            events_per_day: 100_000,
            bytes_per_event: 1_542,
            own_days: 3,
            basis: ErrorBasis::Blended,
            growth: 210 * MB,
            estimate_growth: 154_200_000,
            share_ppm: 60_000,
        });
        f.estimate = Some(Estimate {
            steady_state: steady,
            fill,
            clamped: false,
        });
    }
}

fn text(r: &Report) -> String {
    let mut out = Vec::new();
    r.text(&mut out).unwrap();
    String::from_utf8(out).unwrap()
}

#[test]
fn the_estimate_and_errors_show_in_json_and_text() {
    let mut r = report(
        Some(blended()),
        [Some(day("2026-12-06")), Some(day("2026-12-12"))],
    );
    let fill = Fill {
        days_left: Some(19),
        full_on: Some(day("2026-12-13")),
        beyond_10_years: false,
    };
    with_errors(&mut r, 24 * GB, fill);
    let v = r.json(true);
    assert_eq!(
        v["estimate"],
        json!({"full_on": "2026-12-13", "days_left": 19, "steady_state": "24.0 GB", "beyond_10_years": false, "clamped": false})
    );
    assert_eq!(
        v["basis"]["errors"],
        json!({
            "projects": 2,
            "events_per_day": 100_000,
            "bytes_per_event": 1_542,
            "own_days": 3,
            "basis": "blended",
            "growth_per_day": "210.0 MB",
            "estimate_growth_per_day": "154.2 MB",
        })
    );
    assert_eq!(v["disk"]["beyond_10_years"], json!(false));
    let text = text(&r);
    for line in [
        "growth    up to 873.8 MB of logs and 210.0 MB of errors a day, steady state 28.1 GB\n",
        "forecast  full on 2026-12-06, in 12 days, at up to 873.8 MB of logs and 210.0 MB of errors a day\n",
        // One project keeps its logs as long as the other: the estimate
        // differs by the middle growth only.
        "estimate  full on 2026-12-13, in 19 days, steady state 24.0 GB, at the middle growth of logs\n",
        "errors    100000 events a day from 2 projects at 1542 B each in SQLite, blended: bytes per event measured on 3 of 7 days of SQLite growth here; the bound takes 2100 B each, 210.0 MB a day\n",
    ] {
        assert!(text.contains(line), "{line} in {text}");
    }
}

#[test]
fn a_date_past_ten_years_reads_beyond_in_json_and_text() {
    let mut r = report(Some(blended()), [None, None]);
    let beyond = Fill {
        days_left: None,
        full_on: None,
        beyond_10_years: true,
    };
    with_errors(&mut r, 1_400 * GB, beyond);
    if let Ok(f) = &mut r.forecast {
        f.steady_state = 1_460 * GB;
        f.days_left = None;
        f.full_on = None;
        f.beyond_10_years = true;
    }
    let v = r.json(true);
    assert_eq!(
        (
            &v["disk"]["full_on"],
            &v["disk"]["days_left"],
            &v["disk"]["beyond_10_years"]
        ),
        (&Value::Null, &Value::Null, &json!(true))
    );
    assert_eq!(v["estimate"]["beyond_10_years"], json!(true));
    assert_eq!(
        r.summary(),
        "full beyond 10 years at up to 873.8 MB of logs and 210.0 MB of errors a day, as Davy grows toward 1.5 TB once every retention is full"
    );
    assert!(text(&r).contains("estimate  full beyond 10 years, steady state 1.4 TB,"));
}

#[test]
fn never_with_errors_names_both() {
    let mut r = report(Some(blended()), [None, None]);
    if let Ok(f) = &mut r.forecast {
        f.days_left = None;
        f.full_on = None;
    }
    assert_eq!(
        r.summary(),
        "logs never fill the disk at the current rate: even the longest log retention, 30 days, holds Davy at 28.1 GB"
    );
    with_errors(&mut r, 20 * GB, Fill::new(day("2026-11-24"), None));
    assert_eq!(
        r.summary(),
        "logs and errors never fill the disk at the current rate: even the longest log retention, 30 days, and each project's error retention hold Davy at 28.1 GB"
    );
    assert!(text(&r).contains("estimate  never full at the current rate, steady state 20.0 GB,"));
}

#[test]
fn a_range_without_a_latest_day_says_never_and_a_flat_one_says_nothing() {
    let mut out = Vec::new();
    report(Some(blended()), [Some(day("2026-12-06")), None])
        .text(&mut out)
        .unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains(
        "range     full on 2026-12-06 at the earliest, not within 10 years at the low end, at 873.8 MB to 579.2 MB of logs a day\n"
    ));
    let mut out = Vec::new();
    report(Some(blended()), [Some(day("2026-12-06")); 2])
        .text(&mut out)
        .unwrap();
    assert!(!String::from_utf8(out).unwrap().contains("range"));
}

#[test]
fn a_what_if_of_the_bytes_per_day_has_no_basis() {
    let r = report(None, [Some(day("2026-12-06")); 2]);
    assert_eq!(r.json(true)["basis"], Value::Null);
    let mut out = Vec::new();
    r.text(&mut out).unwrap();
    assert!(!String::from_utf8(out).unwrap().contains("basis"));
}

#[test]
fn a_partial_disk_walk_lowers_the_confidence() {
    let mut r = report(Some(blended()), [None, None]);
    if let Ok(d) = &mut r.disk {
        d.gaps.skipped = vec!["lost+found: permission denied".to_owned()];
        d.gaps.skipped_count = 1;
    }
    assert_eq!(r.json(true)["basis"]["confidence"], json!("low"));
}

/// A server that started counting at 18:00 and is asked at 19:00. shop
/// keeps logs 3 days and wrote 1 GB, api keeps 30 and wrote nothing.
#[test]
fn a_new_server_forecasts_from_the_first_hour_counted() {
    let mut c = Connection::open_in_memory().unwrap();
    store::init(&mut c).unwrap();
    store::create_project(&c, "api").unwrap();
    store::create_project(&c, "shop").unwrap();
    c.execute(
        "UPDATE projects SET log_retention_days=3 WHERE slug='shop'",
        [],
    )
    .unwrap();
    let start_ms = at("2026-11-24T18:00:00Z") / 1_000_000;
    let flush = log_days::Flush {
        start: Some(start_ms),
        segments: vec![log_days::Segment {
            rows: vec![log_days::Row {
                day: day("2026-11-24"),
                project: 2,
                count: Count {
                    records: 1_000,
                    bytes: GB,
                },
            }],
            marks: Vec::new(),
        }],
    };
    stored::write(&c, &flush).unwrap();
    let req = Request {
        project: None,
        since: Some("1h".into()),
        what_if: WhatIf::default(),
        defaults: retention::Defaults::default(),
    };
    let disk = Disk {
        today: 300 * MB,
        total: 500 * GB,
        free: 60 * GB,
        ..Disk::default()
    };
    let r = prepare(&c, &req, at("2026-11-24T19:00:00Z"))
        .unwrap()
        .finish(Ok(disk));
    let v = r.json(true);
    let b = &v["basis"];
    assert_eq!(
        (
            &b["confidence"],
            &b["logs"]["basis"],
            &b["logs"]["raw_per_day"],
            &b["logs"]["counted_hours"],
            &b["logs"]["possible_days"],
            &b["logs"]["footprint"],
        ),
        (
            &json!("low"),
            &json!("first_day"),
            &json!("24.0 GB"),
            &json!(1.0),
            &json!(3),
            &json!(PRIOR.source),
        ),
        "{v:#}"
    );
}

#[test]
fn a_rate_at_the_high_end_of_its_range_reads_up_to() {
    let measured = Logs {
        basis: Basis::Measured,
        growth: Spread::flat(873_772_000),
        ..blended()
    };
    for (logs, rate) in [(blended(), "up to 873.8 MB"), (measured, "873.8 MB")] {
        let r = report(Some(logs), [Some(day("2026-12-06")); 2]);
        let mut out = Vec::new();
        r.text(&mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        for line in [
            format!("growth    {rate} of logs a day, steady state 28.1 GB\n"),
            format!("forecast  full on 2026-12-06, in 12 days, at {rate} of logs a day\n"),
        ] {
            assert!(text.contains(&line), "{line} in {text}");
        }
    }
}

#[test]
fn a_worst_case_range_starts_at_the_earliest() {
    let mut r = report(
        Some(blended()),
        [Some(day("2026-12-06")), Some(day("2026-12-12"))],
    );
    if let Ok(f) = &mut r.forecast {
        f.shortest_days = 3;
    }
    let mut out = Vec::new();
    r.text(&mut out).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(
        text.contains("range     full at the earliest between 2026-12-06 and 2026-12-12, at 873.8 MB to 579.2 MB of logs a day\n"),
        "{text}"
    );
}

/// The ratio may count a day 30 days back, so the raw bytes reach it.
#[test]
fn raw_days_reach_30_days_back() {
    let mut c = Connection::open_in_memory().unwrap();
    store::init(&mut c).unwrap();
    store::create_project(&c, "api").unwrap();
    let row = |d: &str| log_days::Row {
        day: day(d),
        project: 1,
        count: Count {
            records: 1,
            bytes: GB,
        },
    };
    let flush = log_days::Flush {
        start: None,
        segments: vec![log_days::Segment {
            rows: vec![row("2026-10-24"), row("2026-10-25")],
            marks: Vec::new(),
        }],
    };
    stored::write(&c, &flush).unwrap();
    let now = at("2026-11-24T12:00:00Z");
    let kept = retention::all_by_id(&c, retention::Defaults::default(), now / 1_000_000).unwrap();
    assert_eq!(
        raw_days(&c, day("2026-11-24"), &kept, 30).unwrap(),
        [forecast::RawDay {
            day: day("2026-10-25"),
            projects: vec![("api".to_owned(), GB, false)],
        }]
    );
}

#[test]
fn one_day_reads_singular_and_a_clamped_estimate_says_so() {
    let mut r = report(Some(blended()), [Some(day("2026-11-25")); 2]);
    with_errors(&mut r, 20 * GB, Fill::new(day("2026-11-24"), Some(1)));
    if let Ok(f) = &mut r.forecast {
        f.shortest_days = 7;
        f.days_left = Some(1);
        f.full_on = Some(day("2026-11-25"));
        if let Some(e) = &mut f.estimate {
            e.clamped = true;
        }
    }
    assert!(
        r.summary()
            .starts_with("full on 2026-11-25, in 1 day at the earliest, at ")
    );
    assert_eq!(r.json(false)["estimate"]["clamped"], json!(true));
    let text = text(&r);
    let line = "estimate  full on 2026-11-25, in 1 day, the bound's day: its own sums put the disk full earlier, as its ratio puts less on disk than the walk found, steady state 20.0 GB, with each project's logs kept as long as it keeps them and packing alike\n";
    assert!(text.contains(line), "{text}");
}

/// What `prepare` reads for the error forecast: each project's error days
/// of the newest 7 closed ones and today, from its first errors on, the
/// events it keeps, its error retention, and SQLite's samples from 31 days
/// back, the one before the first of the newest 30.
#[test]
fn prepare_reads_error_days_events_and_store_samples() {
    let mut c = Connection::open_in_memory().unwrap();
    store::init(&mut c).unwrap();
    store::create_project(&c, "api").unwrap();
    store::create_project(&c, "shop").unwrap();
    let created_ms = at("2026-09-02T10:16:40Z") / 1_000_000;
    c.execute(
        "UPDATE projects SET created_at=?1, log_retention_days=14, error_retention_days=90 WHERE id=1",
        [created_ms],
    )
    .unwrap();
    c.execute_batch(
        "INSERT INTO usage_daily(day, project_id, signal, records, bytes) VALUES
         ('2026-09-05', 1, 'logs', 1, 10),
         ('2026-09-10', 1, 'errors', 5, 500),
         ('2026-11-16', 1, 'errors', 7, 700),
         ('2026-11-17', 1, 'errors', 3000, 9000000),
         ('2026-11-20', 1, 'logs', 40, 4000),
         ('2026-11-23', 1, 'errors', 100, 300000),
         ('2026-11-16', 2, 'errors', 9, 900),
         ('2026-11-20', 2, 'logs', 60, 6000);
         INSERT INTO issues(id,project_id,grouping_version,fingerprint,title,level,status,first_seen,last_seen,event_count) VALUES
         (1,1,1,'a','A','error','unresolved',0,0,1000),
         (2,1,1,'b','B','error','unresolved',0,0,200),
         (3,2,1,'c','C','error','unresolved',0,0,50);
         INSERT INTO store_days(day, at, bytes, events) VALUES
         ('2026-10-23', 1, 100, 10),
         ('2026-10-24', 2, 200, 20),
         ('2026-11-23', 3, 300, 30);",
    )
    .unwrap();
    let req = Request {
        project: None,
        since: None,
        what_if: WhatIf::default(),
        defaults: retention::Defaults::default(),
    };
    let p = prepare(&c, &req, at("2026-11-24T12:00:00Z")).unwrap();
    assert_eq!(
        p.errors,
        [forecast::ProjectErrors {
            slug: "api".into(),
            created: 1_788_344_200_000_000_000,
            first: day("2026-09-10"),
            daily: vec![
                (
                    day("2026-11-17"),
                    Count {
                        records: 3_000,
                        bytes: 9_000_000,
                    },
                ),
                (
                    day("2026-11-23"),
                    Count {
                        records: 100,
                        bytes: 300_000,
                    },
                ),
            ],
            stored: 1_200,
            days: 90,
        }]
    );
    let sample = |d: &str, at_ms, bytes, events| forecast::StoreDay {
        day: day(d),
        at_ms,
        bytes,
        events,
    };
    assert_eq!(
        p.store,
        [
            sample("2026-10-24", 2, 200, 20),
            sample("2026-11-23", 3, 300, 30),
        ]
    );
}

#[test]
fn each_project_shows_the_error_events_it_discarded_in_the_window() {
    let mut c = Connection::open_in_memory().unwrap();
    store::init(&mut c).unwrap();
    store::create_project(&c, "shop").unwrap();
    for (d, n) in [("2026-11-16", 50), ("2026-11-23", 3), ("2026-11-24", 1)] {
        c.execute(
            "INSERT INTO discarded_daily(day, project_id, records) VALUES (?1, 1, ?2)",
            (d, n),
        )
        .unwrap();
    }
    let req = Request {
        project: None,
        since: None,
        what_if: WhatIf::default(),
        defaults: retention::Defaults::default(),
    };
    let r = prepare(&c, &req, at("2026-11-24T12:00:00Z"))
        .unwrap()
        .finish(Err("not walked".into()));
    assert_eq!(r.json(false)["projects"][0]["errors_discarded"], 4);
    let mut out = Vec::new();
    r.text(&mut out).unwrap();
    let text = String::from_utf8(out).unwrap();
    let line = "shop                 keeps logs 30 days, errors 36500 days, discarded 4 error events received\n";
    assert!(text.contains(line), "{text}");
}
