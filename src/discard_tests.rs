use super::*;
use crate::event::ErrorEvent;
use serde_json::json;

/// 2026-11-25T10:00:00Z in milliseconds.
const NOON_MS: i64 = 1_795_600_800_000;
const DAY_MS: i64 = 86_400_000;

fn memory() -> Connection {
    let mut c = Connection::open_in_memory().unwrap();
    store::init(&mut c).unwrap();
    store::create_project(&c, "shop").unwrap();
    c
}

/// An event of the one error the tests send, received at `received_ms`.
fn event(id: &str, received_ms: i64) -> ErrorEvent {
    error(id, received_ms, "payment declined")
}

/// An event with `message`, received at `received_ms`.
fn error(id: &str, received_ms: i64, message: &str) -> ErrorEvent {
    let mut e = ErrorEvent::parse(json!({
        "event_id": id,
        "message": message,
        "discarded_at": 1,
    }))
    .unwrap();
    e.received_at = received_ms;
    e.timestamp = received_ms;
    e
}

fn number(c: &Connection, sql: &str) -> i64 {
    c.query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn a_discarded_issue_counts_its_new_events_and_stores_none() {
    let mut c = memory();
    store::insert(&c, 1, &event("a0000000000000000000000000000001", NOON_MS)).unwrap();
    // The event's own field of that name is payload, not a mark.
    assert!(
        !discarded(
            &c,
            1,
            1,
            &crate::grouping::group(&event("f0000000000000000000000000000000", 0)).fingerprint
        )
        .unwrap()
    );
    let done = set(&mut c, 1, false, NOON_MS + 1_000).unwrap();
    assert_eq!(
        done,
        Discarded {
            issue: 1,
            project: "shop".into(),
            title: "payment declined".into(),
            discarded: true,
            discarded_at: Some("2026-11-25T10:00:01Z".into()),
        }
    );
    store::insert(
        &c,
        1,
        &event("a0000000000000000000000000000002", NOON_MS + 2_000),
    )
    .unwrap();
    store::insert(
        &c,
        1,
        &event("a0000000000000000000000000000003", NOON_MS + DAY_MS),
    )
    .unwrap();
    assert_eq!(number(&c, "SELECT count(*) FROM events"), 1);
    assert_eq!(number(&c, "SELECT event_count FROM issues WHERE id=1"), 1);
    assert_eq!(number(&c, "SELECT sum(count) FROM issue_hourly"), 1);
    let days: Vec<(String, i64, i64)> = c
        .prepare("SELECT day, project_id, records FROM discarded_daily ORDER BY day")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        days,
        [("2026-11-25".into(), 1, 1), ("2026-11-26".into(), 1, 1)]
    );
    let day = |s: &str| s.parse::<Date>().unwrap();
    let both = between(&c, day("2026-11-25"), day("2026-11-26"), None).unwrap();
    assert_eq!(both, HashMap::from([(1, 2)]));
    let first = between(&c, day("2026-11-25"), day("2026-11-25"), Some(1)).unwrap();
    assert_eq!(first, HashMap::from([(1, 1)]));
    assert_eq!(
        between(&c, day("2026-11-25"), day("2026-11-26"), Some(2)).unwrap(),
        HashMap::new()
    );
}

#[test]
fn undo_stores_the_next_event_again() {
    let mut c = memory();
    store::insert(&c, 1, &event("b0000000000000000000000000000001", NOON_MS)).unwrap();
    set(&mut c, 1, false, NOON_MS).unwrap();
    let undone = set(&mut c, 1, true, NOON_MS + 1).unwrap();
    assert_eq!((undone.discarded, undone.discarded_at), (false, None));
    store::insert(
        &c,
        1,
        &event("b0000000000000000000000000000002", NOON_MS + 5),
    )
    .unwrap();
    assert_eq!(number(&c, "SELECT event_count FROM issues WHERE id=1"), 2);
    assert_eq!(number(&c, "SELECT count(*) FROM discarded_daily"), 0);
}

#[test]
fn discarding_twice_keeps_the_first_time_and_an_unknown_issue_is_refused() {
    let mut c = memory();
    store::insert(&c, 1, &event("c0000000000000000000000000000001", NOON_MS)).unwrap();
    set(&mut c, 1, false, NOON_MS).unwrap();
    let again = set(&mut c, 1, false, NOON_MS + DAY_MS).unwrap();
    assert_eq!(again.discarded_at.as_deref(), Some("2026-11-25T10:00:00Z"));
    assert_eq!(
        set(&mut c, 99, false, NOON_MS).unwrap_err().to_string(),
        "no issue 99"
    );
    assert_eq!(
        set(&mut c, 99, true, NOON_MS).unwrap_err().to_string(),
        "no issue 99"
    );
}

#[test]
fn retention_keeps_a_discarded_issue_after_its_last_event_expires() {
    let mut c = memory();
    store::insert(&c, 1, &event("d0000000000000000000000000000001", NOON_MS)).unwrap();
    set(&mut c, 1, false, NOON_MS).unwrap();
    let later = NOON_MS + 10 * DAY_MS;
    let one_day = crate::retention::Days::new(1).unwrap();
    crate::retention::errors::sweep(&c, later, one_day, crate::retention::errors::BUDGET).unwrap();
    assert_eq!(number(&c, "SELECT count(*) FROM events"), 0);
    assert_eq!(number(&c, "SELECT event_count FROM issues WHERE id=1"), 0);
    // Still discarded: the same error a day later is counted, not stored.
    store::insert(&c, 1, &event("d0000000000000000000000000000002", later)).unwrap();
    assert_eq!(number(&c, "SELECT count(*) FROM events"), 0);
    assert_eq!(number(&c, "SELECT sum(records) FROM discarded_daily"), 1);
}

#[test]
fn the_sweep_prunes_counts_older_than_the_usage_counts() {
    let c = memory();
    for day in ["2025-10-20", "2025-10-21", "2026-11-25"] {
        c.execute(
            "INSERT INTO discarded_daily(day, project_id, records) VALUES (?1, 1, 3)",
            [day],
        )
        .unwrap();
    }
    // 400 days back from 2026-11-25 is 2025-10-21, which goes too.
    prune(&c, NOON_MS).unwrap();
    assert_eq!(number(&c, "SELECT count(*) FROM discarded_daily"), 1);
}

#[test]
fn the_cli_discards_and_undoes_and_prints_the_issue_as_json() {
    let data = crate::test_support::scratch("discard-cli");
    let c = store::open(&data).unwrap();
    store::create_project(&c, "shop").unwrap();
    let title = "bot \u{9b}2J\u{202E}gnp.exe";
    store::insert(
        &c,
        1,
        &error("e0000000000000000000000000000001", NOON_MS, title),
    )
    .unwrap();
    let run_it = |undo| {
        let mut out = Vec::new();
        let command = Issue::Discard {
            issue_id: 1,
            data: data.clone(),
            undo,
        };
        run(&mut out, command).unwrap();
        String::from_utf8(out).unwrap()
    };
    let discarded = run_it(false);
    let at: Option<i64> = c
        .query_row("SELECT discarded_at FROM issues WHERE id=1", [], |r| {
            r.get(0)
        })
        .unwrap();
    let at = time::rfc3339_ms(at.unwrap());
    assert_eq!(
        discarded,
        format!(
            "{{\"issue\":1,\"project\":\"shop\",\"title\":\"bot \\u009b2J\\u202egnp.exe\",\"discarded\":true,\"discarded_at\":\"{at}\"}}\n"
        )
    );
    store::insert(
        &c,
        1,
        &error("e0000000000000000000000000000002", NOON_MS + 1, title),
    )
    .unwrap();
    assert_eq!(number(&c, "SELECT count(*) FROM events"), 1);
    assert_eq!(number(&c, "SELECT sum(records) FROM discarded_daily"), 1);
    assert_eq!(
        run_it(true),
        "{\"issue\":1,\"project\":\"shop\",\"title\":\"bot \\u009b2J\\u202egnp.exe\",\"discarded\":false,\"discarded_at\":null}\n"
    );
    assert_eq!(
        number(&c, "SELECT count(*) FROM issues WHERE discarded_at IS NULL"),
        1
    );
    std::fs::remove_dir_all(&data).unwrap();
}

#[test]
fn a_resent_event_of_a_discarded_issue_counts_once_per_send() {
    let mut c = memory();
    store::insert(&c, 1, &event("f1000000000000000000000000000001", NOON_MS)).unwrap();
    set(&mut c, 1, false, NOON_MS).unwrap();
    // A discarded event leaves no row to find a resend by, so an SDK that
    // retries one event 3 times counts 3.
    for _ in 0..3 {
        store::insert(
            &c,
            1,
            &event("f1000000000000000000000000000002", NOON_MS + 5),
        )
        .unwrap();
    }
    assert_eq!(number(&c, "SELECT sum(records) FROM discarded_daily"), 3);
    // The stored event's resend is still dropped.
    store::insert(
        &c,
        1,
        &event("f1000000000000000000000000000001", NOON_MS + 6),
    )
    .unwrap();
    assert_eq!(number(&c, "SELECT sum(records) FROM discarded_daily"), 3);
}

#[test]
fn an_event_of_a_discarded_issue_sends_no_alert() {
    let mut c = memory();
    c.execute(
        "INSERT INTO channels(project_id, kind, url, created_at) VALUES (1, 'slack', 'https://hooks.slack.com/x', 0)",
        [],
    )
    .unwrap();
    store::insert(&c, 1, &event("f2000000000000000000000000000001", NOON_MS)).unwrap();
    assert_eq!(number(&c, "SELECT count(*) FROM notifications"), 1);
    // Resolved, an event would reopen it and alert; discarded, it does not.
    c.execute(
        "UPDATE issues SET status='resolved', resolved_at=?1 WHERE id=1",
        [NOON_MS],
    )
    .unwrap();
    set(&mut c, 1, false, NOON_MS).unwrap();
    store::insert(
        &c,
        1,
        &event("f2000000000000000000000000000002", NOON_MS + 5),
    )
    .unwrap();
    assert_eq!(number(&c, "SELECT count(*) FROM notifications"), 1);
    assert_eq!(
        number(&c, "SELECT status='resolved' FROM issues WHERE id=1"),
        1
    );
    // With no event left, an event would start the issue anew and alert.
    let later = NOON_MS + 10 * DAY_MS;
    let one_day = crate::retention::Days::new(1).unwrap();
    crate::retention::errors::sweep(&c, later, one_day, crate::retention::errors::BUDGET).unwrap();
    store::insert(&c, 1, &event("f2000000000000000000000000000003", later)).unwrap();
    assert_eq!(number(&c, "SELECT count(*) FROM notifications"), 1);
    assert_eq!(number(&c, "SELECT sum(records) FROM discarded_daily"), 2);
}

#[test]
fn undo_forgets_an_issue_with_no_event_and_no_feedback() {
    let mut c = memory();
    store::insert(&c, 1, &event("f3000000000000000000000000000001", NOON_MS)).unwrap();
    store::insert(
        &c,
        1,
        &error("f3000000000000000000000000000002", NOON_MS, "card expired"),
    )
    .unwrap();
    set(&mut c, 1, false, NOON_MS).unwrap();
    set(&mut c, 2, false, NOON_MS).unwrap();
    let later = NOON_MS + 10 * DAY_MS;
    let one_day = crate::retention::Days::new(1).unwrap();
    crate::retention::errors::sweep(&c, later, one_day, crate::retention::errors::BUDGET).unwrap();
    assert_eq!(
        number(&c, "SELECT count(*) FROM issues WHERE event_count=0"),
        2
    );
    // Feedback keeps issue 2 after the undo, as it keeps an issue without
    // a mark.
    c.execute(
        "INSERT INTO user_feedback(project_id, received_at, issue_id, message) VALUES (1, ?1, 2, 'broken')",
        [later],
    )
    .unwrap();
    let undone = set(&mut c, 1, true, later).unwrap();
    assert_eq!(
        (undone.title.as_str(), undone.discarded),
        ("payment declined", false)
    );
    set(&mut c, 2, true, later).unwrap();
    assert_eq!(number(&c, "SELECT id FROM issues"), 2);
    assert_eq!(number(&c, "SELECT id FROM id_floor WHERE name='issues'"), 1);
    // The error comes back as a new issue, with an id never used before.
    store::insert(&c, 1, &event("f3000000000000000000000000000003", later)).unwrap();
    assert_eq!(number(&c, "SELECT max(id) FROM issues"), 3);
    // An issue that keeps events is not forgotten.
    set(&mut c, 3, false, later).unwrap();
    set(&mut c, 3, true, later).unwrap();
    assert_eq!(number(&c, "SELECT count(*) FROM issues"), 2);
}
