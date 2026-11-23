use super::*;

/// 2024-10-02T07:00:00Z, the start of an hour.
const HOUR: Timestamp = 1_727_852_400_000;
const SID: &str = "6e3a9a0bd0a14e4f8a6e1c2d3b4a5f60";

fn memory() -> Connection {
    let mut c = Connection::open_in_memory().unwrap();
    crate::store::init(&mut c).unwrap();
    crate::store::create_project(&c, "shop").unwrap();
    crate::store::create_project(&c, "admin").unwrap();
    c
}

/// What the browser SDK sends, with `status` and `errors` filled in.
fn update(status: &str, errors: u64) -> Vec<u8> {
    format!(
        r#"{{"sid": "{SID}", "init": {}, "started": "2024-10-02T07:12:30.250Z",
            "timestamp": "2024-10-02T07:15:00.000Z", "status": "{status}", "errors": {errors},
            "did": "user-42", "duration": 150.5,
            "attrs": {{"release": "shop@1.4.0", "environment": "staging", "user_agent": "Mozilla/5.0"}}}}"#,
        status == "ok" && errors == 0
    )
    .into_bytes()
}

fn session(release: &str, sid: u8, started: Timestamp) -> Session {
    Session {
        sid: [sid; 16],
        started,
        flags: 0,
        user: None,
        release: release.to_owned(),
        environment: "production".to_owned(),
    }
}

type Row = (Timestamp, String, String, i64, i64, i64, i64);

fn rows(c: &Connection) -> Vec<Row> {
    let mut q = c
        .prepare(
            "SELECT hour, release, environment, started, errored, crashed, abnormal
             FROM release_sessions ORDER BY project_id, hour, release",
        )
        .unwrap();
    q.query_map([], |r| {
        Ok((
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            r.get(3)?,
            r.get(4)?,
            r.get(5)?,
            r.get(6)?,
        ))
    })
    .unwrap()
    .collect::<rusqlite::Result<_>>()
    .unwrap()
}

fn row(release: &str, started: i64, errored: i64, crashed: i64, abnormal: i64) -> Row {
    (
        HOUR,
        release.to_owned(),
        "staging".to_owned(),
        started,
        errored,
        crashed,
        abnormal,
    )
}

#[test]
fn parse_session_reads_what_the_browser_sdk_sends() {
    let mut sid = [0; 16];
    sid.copy_from_slice(&uuid::Uuid::parse_str(SID).unwrap().into_bytes());
    assert_eq!(
        parse_session(&update("ok", 0)),
        Some(Session {
            sid,
            started: HOUR + 750_250,
            flags: 0,
            user: Some("user-42".to_owned()),
            release: "shop@1.4.0".to_owned(),
            environment: "staging".to_owned(),
        })
    );
    let flags = |status, errors| parse_session(&update(status, errors)).unwrap().flags;
    assert_eq!(flags("ok", 1), ERRORED);
    assert_eq!(flags("exited", 0), 0);
    assert_eq!(flags("errored", 0), ERRORED);
    assert_eq!(flags("crashed", 1), ERRORED | CRASHED);
    assert_eq!(flags("abnormal", 0), ABNORMAL);
}

#[test]
fn parse_session_refuses_what_it_cannot_count() {
    let without = |field: &str, with: &str| {
        let payload = String::from_utf8(update("ok", 0)).unwrap();
        parse_session(payload.replace(field, with).as_bytes())
    };
    assert_eq!(without(SID, "not-a-uuid"), None);
    assert_eq!(without("2024-10-02T07:12:30.250Z", "yesterday"), None);
    assert_eq!(without("shop@1.4.0", " "), None);
    assert_eq!(without("shop@1.4.0", "shop\\u0000"), None);
    assert_eq!(without("shop@1.4.0", &"r".repeat(201)), None);
    assert_eq!(parse_session(b"[]"), None);
    let plain = format!(
        r#"{{"sid": "{SID}", "started": "2024-10-02T07:12:30Z", "attrs": {{"release": "1.0"}}}}"#
    );
    let s = parse_session(plain.as_bytes()).unwrap();
    assert_eq!(
        (s.environment.as_str(), s.flags, s.user),
        ("production", 0, None)
    );
}

#[test]
fn parse_aggregates_reads_what_server_sdks_send() {
    let payload = br#"{"aggregates": [
        {"started": "2024-10-02T07:12:00.000Z", "exited": 50, "errored": 3, "crashed": 1},
        {"started": "2024-10-02T07:13:00.000Z", "exited": 0},
        {"started": "never", "exited": 4},
        {"started": "2024-10-02T07:14:00.000Z", "did": "u1", "abnormal": 2, "crashed": 99000000}
    ], "attrs": {"release": "api@2", "environment": "production"}}"#;
    assert_eq!(
        parse_aggregates(payload),
        Some(Aggregates {
            release: "api@2".to_owned(),
            environment: "production".to_owned(),
            counts: vec![
                Count {
                    started_at: HOUR + 720_000,
                    user: None,
                    started: 54,
                    errored: 4,
                    crashed: 1,
                    abnormal: 0,
                },
                Count {
                    started_at: HOUR + 840_000,
                    user: Some("u1".to_owned()),
                    started: 1_000_002,
                    errored: 1_000_000,
                    crashed: 1_000_000,
                    abnormal: 2,
                },
            ],
        })
    );
    assert_eq!(parse_aggregates(br#"{"aggregates": []}"#), None);
}

#[test]
fn parse_aggregates_reads_at_most_a_hundred_counts() {
    let counts: Vec<String> = (0..150)
        .map(|_| r#"{"started": "2024-10-02T07:12:00Z", "exited": 1}"#.to_owned())
        .collect();
    let payload = format!(
        r#"{{"aggregates": [{}], "attrs": {{"release": "1"}}}}"#,
        counts.join(",")
    );
    assert_eq!(
        parse_aggregates(payload.as_bytes()).unwrap().counts.len(),
        100
    );
}

#[test]
fn add_session_counts_each_session_once_however_many_updates_come() {
    let c = memory();
    let at = HOUR + 1_000_000;
    for (status, errors) in [
        ("ok", 0),
        ("ok", 0),
        ("ok", 1),
        ("crashed", 1),
        ("crashed", 1),
    ] {
        let s = parse_session(&update(status, errors)).unwrap();
        assert_eq!(add_session(&c, 1, at, &s).unwrap(), None);
    }
    assert_eq!(rows(&c), vec![row("shop@1.4.0", 1, 1, 1, 0)]);
}

#[test]
fn add_session_counts_a_session_first_seen_after_its_start() {
    let c = memory();
    let s = parse_session(&update("abnormal", 0)).unwrap();
    add_session(&c, 1, HOUR + 1_000_000, &s).unwrap();
    assert_eq!(rows(&c), vec![row("shop@1.4.0", 1, 0, 0, 1)]);
}

#[test]
fn add_session_refuses_a_start_outside_the_window() {
    let c = memory();
    let s = session("1", 1, HOUR);
    assert_eq!(
        add_session(&c, 1, HOUR + MAX_AGE_MS + 1, &s).unwrap(),
        Some(OUT_OF_WINDOW)
    );
    assert_eq!(
        add_session(&c, 1, HOUR - MAX_AHEAD_MS - 1, &s).unwrap(),
        Some(OUT_OF_WINDOW)
    );
    assert_eq!(add_session(&c, 1, HOUR + MAX_AGE_MS, &s).unwrap(), None);
    assert_eq!(rows(&c).len(), 1);
}

#[test]
fn add_session_refuses_new_sessions_past_the_daily_cap_but_counts_old_ones() {
    let c = memory();
    add_session(&c, 1, HOUR, &session("1", 1, HOUR)).unwrap();
    c.execute("UPDATE release_sessions SET started=?1", [MAX_PER_DAY])
        .unwrap();
    assert_eq!(
        add_session(&c, 1, HOUR, &session("1", 2, HOUR)).unwrap(),
        Some(OVER_DAILY_CAP)
    );
    assert_eq!(
        add_session(&c, 2, HOUR, &session("1", 2, HOUR)).unwrap(),
        None
    );
    let crashed = Session {
        flags: ERRORED | CRASHED,
        ..session("1", 1, HOUR)
    };
    assert_eq!(add_session(&c, 1, HOUR, &crashed).unwrap(), None);
    let tomorrow = HOUR + DAY_MS;
    assert_eq!(
        add_session(&c, 1, tomorrow, &session("1", 3, tomorrow)).unwrap(),
        None
    );
    let started: Vec<(i64, i64)> = rows(&c).iter().map(|r| (r.3, r.5)).collect();
    assert_eq!(started, vec![(MAX_PER_DAY, 1), (1, 0), (1, 0)]);
}

#[test]
fn add_session_keeps_at_most_a_hundred_releases_an_hour() {
    let c = memory();
    for i in 0..MAX_PAIRS {
        let sid = u8::try_from(i).unwrap();
        let s = session(&format!("r{i}"), sid, HOUR);
        assert_eq!(add_session(&c, 1, HOUR, &s).unwrap(), None, "release {i}");
    }
    let one_more = session("r100", 200, HOUR);
    assert_eq!(
        add_session(&c, 1, HOUR, &one_more).unwrap(),
        Some(OVER_DAILY_CAP)
    );
    assert_eq!(
        add_session(&c, 1, HOUR, &session("r0", 201, HOUR)).unwrap(),
        None
    );
    let later = HOUR + HOUR_MS;
    assert_eq!(
        add_session(&c, 1, later, &session("r100", 200, later)).unwrap(),
        None
    );
}

#[test]
fn a_user_counts_once_a_day_and_per_project() {
    let c = memory();
    let mut s = parse_session(&update("ok", 0)).unwrap();
    add_session(&c, 1, HOUR, &s).unwrap();
    s.sid = [9; 16];
    s.flags = ERRORED | CRASHED;
    add_session(&c, 1, HOUR, &s).unwrap();
    add_session(&c, 2, HOUR, &s).unwrap();
    let users: Vec<(i64, i64, i64)> = c
        .prepare("SELECT project_id, user, flags FROM release_users ORDER BY project_id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(users.len(), 2);
    assert_eq!((users[0].0, users[0].2), (1, ERRORED | CRASHED));
    assert_eq!(users[0].1, user_key(1, "user-42"));
    assert_ne!(users[0].1, users[1].1, "the key differs per project");
}

#[test]
fn items_add_counts_aggregates_and_reports_what_it_refused() {
    let c = memory();
    let count = |started_at| Count {
        started_at,
        started: 10,
        errored: 2,
        crashed: 1,
        ..Count::default()
    };
    let a = Aggregates {
        release: "api@2".to_owned(),
        environment: "staging".to_owned(),
        counts: vec![count(HOUR), count(HOUR + 60_000), count(HOUR - 6 * DAY_MS)],
    };
    crate::items::add(&c, 1, HOUR, &[crate::items::SdkItem::Sessions(a)]).unwrap();
    assert_eq!(rows(&c), vec![row("api@2", 20, 4, 2, 0)]);
    let refused: (String, String, i64) = c
        .query_row(
            "SELECT reason, category, quantity FROM client_reports",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        refused,
        ("davy_out_of_window".to_owned(), "session".to_owned(), 10)
    );
}

#[test]
fn a_session_keeps_its_first_start_however_its_updates_move_it() {
    let c = memory();
    let first = session("1", 1, HOUR);
    assert_eq!(add_session(&c, 1, HOUR, &first).unwrap(), None);
    // A later update that claims a fresh start and an error.
    let later = HOUR + 4 * DAY_MS;
    let moved = Session {
        flags: 2,
        ..session("1", 1, later)
    };
    assert_eq!(add_session(&c, 1, later, &moved).unwrap(), None);
    let counted = (HOUR, "1".to_owned(), "production".to_owned(), 1, 1, 0, 0);
    assert_eq!(rows(&c), vec![counted.clone()]);
    // The row goes when the first start leaves the window, not 5 days after
    // the last update, and the same session is not counted again after it.
    let gone = HOUR + 5 * DAY_MS + 1;
    prune(&c, gone).unwrap();
    let seen: i64 = c
        .query_row("SELECT count(*) FROM session_seen", [], |r| r.get(0))
        .unwrap();
    assert_eq!(seen, 0);
    assert_eq!(
        add_session(&c, 1, gone, &first).unwrap(),
        Some(OUT_OF_WINDOW)
    );
    assert_eq!(rows(&c), vec![counted]);
}

#[test]
fn prune_forgets_sessions_out_of_the_window_and_deletes_old_counts() {
    let c = memory();
    let now = HOUR + 100 * DAY_MS;
    let edge = now - 5 * DAY_MS;
    add_session(&c, 1, edge - 1, &session("1", 1, edge - 1)).unwrap();
    add_session(&c, 1, edge, &session("1", 2, edge)).unwrap();
    let old = now - 91 * DAY_MS;
    c.execute(
        "INSERT INTO release_sessions VALUES (1, ?1, 'old', 'production', 1, 0, 0, 0)",
        [old],
    )
    .unwrap();
    c.execute(
        "INSERT INTO release_users VALUES (?1, 1, 'old', 'production', 7, 0)",
        [old.div_euclid(DAY_MS) * DAY_MS],
    )
    .unwrap();
    prune(&c, now).unwrap();
    let count = |table: &str| -> i64 {
        c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(count("session_seen"), 1);
    assert_eq!(count("release_sessions"), 2);
    assert_eq!(count("release_users"), 0);
}
