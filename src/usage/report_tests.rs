use super::*;
use crate::{
    logs::store::log_days,
    store,
    usage::{
        footprint::{Band, FIXTURE, PRIOR},
        forecast::{Basis, Logs, Spread},
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

#[test]
fn a_range_without_a_latest_day_says_never_and_a_flat_one_says_nothing() {
    let mut out = Vec::new();
    report(Some(blended()), [Some(day("2026-12-06")), None])
        .text(&mut out)
        .unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains(
        "range     full on 2026-12-06 at the earliest, never at the low end, at 873.8 MB to 579.2 MB of logs a day\n"
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
        raw_days(&c, day("2026-11-24"), &kept).unwrap(),
        [forecast::RawDay {
            day: day("2026-10-25"),
            projects: vec![("api".to_owned(), GB, false)],
        }]
    );
}
