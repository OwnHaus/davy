use super::*;
use crate::store;

fn at(s: &str) -> i64 {
    time::nanos(s.parse().unwrap()).unwrap()
}

/// 2026-11-23 to 2026-11-25, with two projects and a row of each table on
/// each side of the window.
fn memory() -> Connection {
    let mut c = Connection::open_in_memory().unwrap();
    store::init(&mut c).unwrap();
    store::create_project(&c, "shop").unwrap();
    store::create_project(&c, "api").unwrap();
    let rows: [(&str, (&str, i64, i64)); 7] = [
        (
            "INSERT INTO log_days(day, project_id, records, bytes) VALUES (?1, ?2, 1, ?3)",
            ("2026-11-22", 1, 7),
        ),
        (
            "INSERT INTO log_days(day, project_id, records, bytes) VALUES (?1, ?2, 1, ?3)",
            ("2026-11-23", 1, 100),
        ),
        (
            "INSERT INTO log_days(day, project_id, records, bytes) VALUES (?1, ?2, 1, ?3)",
            ("2026-11-25", 1, 40),
        ),
        // Indexed for tomorrow, counted on today.
        (
            "INSERT INTO log_days(day, project_id, records, bytes) VALUES (?1, ?2, 1, ?3)",
            ("2026-11-26", 1, 2),
        ),
        (
            "INSERT INTO log_days(day, project_id, records, bytes) VALUES (?1, ?2, 1, ?3)",
            ("2026-11-24", 2, 9),
        ),
        (
            "INSERT INTO discarded_daily(day, project_id, records) VALUES (?1, ?2, ?3)",
            ("2026-11-24", 1, 3),
        ),
        (
            "INSERT INTO discarded_daily(day, project_id, records) VALUES (?1, ?2, ?3)",
            ("2026-11-21", 1, 50),
        ),
    ];
    for (sql, row) in rows {
        c.execute(sql, row).unwrap();
    }
    for (day, project, signal, records, bytes) in [
        ("2026-11-23", 1, "logs", 10, 100),
        ("2026-11-23", 1, "errors", 2, 5_000),
        ("2026-11-24", 2, "checkins", 4, 400),
        ("2026-11-20", 1, "errors", 9, 9_000),
    ] {
        c.execute(
            "INSERT INTO usage_daily(day, project_id, signal, records, bytes) VALUES (?1, ?2, ?3, ?4, ?5)",
            (day, project, signal, records, bytes),
        )
        .unwrap();
    }
    for (day, rule, live, records) in [("2026-11-25", 1, 1, 6), ("2026-11-25", 2, 0, 70)] {
        c.execute(
            "INSERT INTO drop_daily(day, rule_id, live, project_id, records, bytes) VALUES (?1, ?2, ?3, 1, ?4, 1)",
            (day, rule, live, records),
        )
        .unwrap();
    }
    for (day, project, reason, category, quantity) in [
        ("2026-11-23T00:00:00Z", 1, "davy_rate", "monitor", 5),
        ("2026-11-23T00:00:00Z", 1, "davy_daily_cap", "session", 1),
        ("2026-11-24T00:00:00Z", 1, "davy_too_large", "monitor", 2),
        ("2026-11-24T00:00:00Z", 1, "davy_bad_slug", "monitor", 3),
        ("2026-11-25T00:00:00Z", 2, "queue_overflow", "log_item", 8),
        ("2026-11-24T00:00:00Z", 2, "davy_quota", "error", 7),
        // Bytes, not items: left out.
        ("2026-11-25T00:00:00Z", 2, "sample_rate", "log_byte", 5_000),
        ("2026-11-22T00:00:00Z", 1, "davy_rate", "monitor", 99),
    ] {
        c.execute(
            "INSERT INTO client_reports(day, project_id, reason, category, quantity) VALUES (?1, ?2, ?3, ?4, ?5)",
            (at(day) / 1_000_000, project, reason, category, quantity),
        )
        .unwrap();
    }
    c
}

#[test]
fn every_project_gets_each_count_per_day_of_the_window() {
    let c = memory();
    let got = days(
        &c,
        None,
        at("2026-11-23T08:00:00Z"),
        at("2026-11-25T12:00:00Z"),
    )
    .unwrap();
    assert_eq!(
        got,
        UsageDays {
            days: vec![
                "2026-11-23".to_owned(),
                "2026-11-24".to_owned(),
                "2026-11-25".to_owned()
            ],
            names: vec!["shop".to_owned(), "api".to_owned()],
            projects: vec![
                ProjectDays {
                    project: "shop".to_owned(),
                    counts: Counts {
                        stored: vec![5_100, 0, 42],
                        accepted: vec![12, 0, 0],
                        filtered: vec![0, 0, 6],
                        discarded: vec![0, 3, 0],
                        limited: vec![6, 0, 0],
                        refused: vec![0, 3, 0],
                        unreadable: vec![0, 2, 0],
                        sdk_dropped: vec![0, 0, 0],
                    },
                },
                ProjectDays {
                    project: "api".to_owned(),
                    counts: Counts {
                        stored: vec![0, 409, 0],
                        accepted: vec![0, 4, 0],
                        filtered: vec![0, 0, 0],
                        discarded: vec![0, 0, 0],
                        limited: vec![0, 7, 0],
                        refused: vec![0, 0, 0],
                        unreadable: vec![0, 0, 0],
                        sdk_dropped: vec![0, 0, 8],
                    },
                },
            ],
            other: None,
        }
    );
}

#[test]
fn one_project_reads_only_its_own_rows() {
    let c = memory();
    let got = days(
        &c,
        Some("api"),
        at("2026-11-24T00:00:00Z"),
        at("2026-11-25T00:00:00Z"),
    )
    .unwrap();
    assert_eq!(got.days, ["2026-11-24", "2026-11-25"]);
    assert_eq!(got.names, ["shop", "api"]);
    assert_eq!(got.projects.len(), 1);
    assert_eq!(got.projects[0].project, "api");
    assert_eq!(got.projects[0].counts.stored, [409, 0]);
    assert_eq!(got.projects[0].counts.sdk_dropped, [0, 8]);
    assert_eq!(got.other, None);
}

#[test]
fn an_unknown_project_is_refused_by_name() {
    let c = memory();
    let err = days(
        &c,
        Some("nope"),
        at("2026-11-24T00:00:00Z"),
        at("2026-11-25T00:00:00Z"),
    )
    .unwrap_err();
    assert_eq!(err.to_string(), "unknown project \"nope\"");
}

#[test]
fn a_server_without_projects_has_days_and_no_rows() {
    let mut c = Connection::open_in_memory().unwrap();
    store::init(&mut c).unwrap();
    let got = days(
        &c,
        None,
        at("2026-11-25T00:00:00Z"),
        at("2026-11-25T23:00:00Z"),
    )
    .unwrap();
    assert_eq!(
        got,
        UsageDays {
            days: vec!["2026-11-25".to_owned()],
            names: Vec::new(),
            projects: Vec::new(),
            other: None,
        }
    );
}

/// Twelve projects `p01` to `p12`, project `n` storing `n` bytes of errors
/// on 2026-11-25 and `p12` also 1 byte of logs on 2026-11-24.
fn many() -> Connection {
    let mut c = Connection::open_in_memory().unwrap();
    store::init(&mut c).unwrap();
    for n in 1..=12 {
        let (id, _) = store::create_project(&c, &format!("p{n:02}")).unwrap();
        c.execute(
            "INSERT INTO usage_daily(day, project_id, signal, records, bytes) VALUES ('2026-11-25', ?1, 'errors', 1, ?2)",
            (id, n),
        )
        .unwrap();
    }
    c.execute(
        "INSERT INTO log_days(day, project_id, records, bytes) VALUES ('2026-11-24', 12, 1, 1)",
        [],
    )
    .unwrap();
    c
}

#[test]
fn past_eight_projects_the_rest_are_summed_as_other_with_the_top_three_named() {
    let got = days(
        &many(),
        None,
        at("2026-11-24T00:00:00Z"),
        at("2026-11-25T00:00:00Z"),
    )
    .unwrap();
    assert_eq!(got.names.len(), 12);
    assert_eq!(
        got.projects
            .iter()
            .map(|p| p.project.as_str())
            .collect::<Vec<_>>(),
        ["p01", "p02", "p03", "p04", "p05", "p06", "p07", "p08"]
    );
    assert_eq!(
        got.other,
        Some(OtherDays {
            projects: 4,
            top: vec![
                ProjectStored {
                    project: "p12".to_owned(),
                    stored: vec![1, 12],
                },
                ProjectStored {
                    project: "p11".to_owned(),
                    stored: vec![0, 11],
                },
                ProjectStored {
                    project: "p10".to_owned(),
                    stored: vec![0, 10],
                },
            ],
            counts: Counts {
                stored: vec![1, 42],
                accepted: vec![0, 4],
                filtered: vec![0, 0],
                discarded: vec![0, 0],
                limited: vec![0, 0],
                refused: vec![0, 0],
                unreadable: vec![0, 0],
                sdk_dropped: vec![0, 0],
            },
        })
    );
}

#[test]
fn a_project_past_the_eighth_asked_for_alone_has_no_other() {
    let got = days(
        &many(),
        Some("p11"),
        at("2026-11-25T00:00:00Z"),
        at("2026-11-25T00:00:00Z"),
    )
    .unwrap();
    assert_eq!(got.projects.len(), 1);
    assert_eq!(got.projects[0].project, "p11");
    assert_eq!(got.projects[0].counts.stored, [11]);
    assert_eq!(got.other, None);
}

#[test]
fn a_client_report_with_davy_s_own_reasons_adds_to_no_count_of_davy() {
    let mut c = Connection::open_in_memory().unwrap();
    store::init(&mut c).unwrap();
    store::create_project(&c, "shop").unwrap();
    let payload = br#"{"discarded_events": [
        {"reason": "davy_rate", "category": "monitor", "quantity": 1000},
        {"reason": "davy_bad_slug", "category": "monitor", "quantity": 1000},
        {"reason": "davy_invalid", "category": "monitor", "quantity": 1000},
        {"reason": "queue_overflow", "category": "log_item", "quantity": 1},
        {"reason": "queue_overflow", "category": "log_byte", "quantity": 5000}
    ]}"#;
    let entries = crate::items::client_report::parse(payload).unwrap();
    let now = at("2026-11-25T12:00:00Z");
    crate::items::client_report::add(&c, 1, now / 1_000_000, &entries).unwrap();
    let got = days(&c, None, now, now).unwrap();
    let counts = &got.projects[0].counts;
    assert_eq!(counts.limited, [0]);
    assert_eq!(counts.refused, [0]);
    assert_eq!(counts.unreadable, [0]);
    assert_eq!(counts.sdk_dropped, [1]);
}
