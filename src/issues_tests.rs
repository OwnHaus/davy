use super::*;
use crate::{event::ErrorEvent, store};
use serde_json::{Value, json};

/// Half past an hour, so the newest hour of a chart is half over.
const NOW: Timestamp = 1_763_812_800_000 + 30 * 60_000;
const H: Timestamp = HOUR_MS;

fn memory() -> Connection {
    let mut c = Connection::open_in_memory().unwrap();
    store::init(&mut c).unwrap();
    store::create_project(&c, "shop").unwrap();
    store::create_project(&c, "blog").unwrap();
    c
}

fn add(c: &Connection, project: i64, message: &str, at: Timestamp, extra: Value) {
    let mut raw = json!({ "message": message, "timestamp": format!("{}", jiff::Timestamp::from_millisecond(at).unwrap()) });
    if let (Some(raw), Value::Object(extra)) = (raw.as_object_mut(), extra) {
        raw.extend(extra);
    }
    let e = ErrorEvent {
        received_at: at,
        ..ErrorEvent::parse(raw).unwrap()
    };
    store::insert(c, project, &e).unwrap();
}

/// alpha: 2 events, first 5 h ago, last 1 h ago, in prod on v1.
/// beta: 1 event 30 min ago, in staging. gamma: 3 events 3 h and 2 h ago,
/// on v2. delta: 1 event in the other project.
fn three_issues() -> Connection {
    let c = memory();
    add(
        &c,
        1,
        "alpha failed",
        NOW - 5 * H,
        json!({"environment":"prod","release":"v1"}),
    );
    add(
        &c,
        1,
        "alpha failed",
        NOW - H,
        json!({"environment":"prod"}),
    );
    add(
        &c,
        1,
        "beta failed",
        NOW - H / 2,
        json!({"environment":"staging"}),
    );
    add(&c, 1, "gamma failed", NOW - 3 * H, json!({"release":"v2"}));
    add(&c, 1, "gamma failed", NOW - 2 * H, json!({}));
    add(&c, 1, "gamma failed", NOW - 2 * H, json!({}));
    add(&c, 2, "delta failed", NOW - 4 * H, json!({}));
    c
}

fn titles(c: &Connection, filter: &Filter, sort: Sort) -> Vec<String> {
    let page = list(c, filter, sort, None, NOW).unwrap();
    page.issues.into_iter().map(|i| i.title).collect()
}

#[test]
fn each_sort_orders_the_list_newest_or_biggest_first() {
    let c = three_issues();
    let shop = Filter {
        project: Some(1),
        ..Filter::default()
    };
    assert_eq!(
        titles(&c, &shop, Sort::LastSeen),
        ["beta failed", "alpha failed", "gamma failed"]
    );
    assert_eq!(
        titles(&c, &shop, Sort::Events),
        ["gamma failed", "alpha failed", "beta failed"]
    );
    assert_eq!(
        titles(&c, &shop, Sort::FirstSeen),
        ["beta failed", "gamma failed", "alpha failed"]
    );
}

#[test]
fn a_row_counts_its_events_per_hour_up_to_the_current_one() {
    let c = three_issues();
    let page = list(&c, &Filter::default(), Sort::Events, None, NOW).unwrap();
    assert_eq!(page.hours_from, "2025-11-21T13:00:00Z");
    let cells = |title: &str| {
        let row = page.issues.iter().find(|i| i.title == title).unwrap();
        row.hourly
            .iter()
            .enumerate()
            .filter(|(_, n)| **n > 0)
            .map(|(cell, n)| (cell, *n))
            .collect::<Vec<_>>()
    };
    assert_eq!(cells("alpha failed"), [(18, 1), (22, 1)]);
    assert_eq!(cells("beta failed"), [(23, 1)]);
    assert_eq!(cells("gamma failed"), [(20, 1), (21, 2)]);
    let gamma = page
        .issues
        .iter()
        .find(|i| i.title == "gamma failed")
        .unwrap();
    assert_eq!(
        (gamma.count, gamma.status, gamma.project.as_str()),
        (3, Status::Unresolved, "shop")
    );
    assert_eq!(gamma.first_seen, "2025-11-22T09:30:00Z");
    assert_eq!(gamma.last_seen, "2025-11-22T10:30:00Z");
    assert_eq!(hourly(&c, gamma.id, hours_from(NOW)).unwrap(), gamma.hourly);
}

#[test]
fn every_filter_narrows_the_list() {
    let c = three_issues();
    let only = |filter: Filter| titles(&c, &filter, Sort::LastSeen);
    assert_eq!(only(Filter::default()).len(), 4);
    assert_eq!(
        only(Filter {
            project: Some(2),
            ..Filter::default()
        }),
        ["delta failed"]
    );
    assert_eq!(
        only(Filter {
            environment: Some("prod".to_owned()),
            ..Filter::default()
        }),
        ["alpha failed"]
    );
    assert_eq!(
        only(Filter {
            release: Some("v2".to_owned()),
            ..Filter::default()
        }),
        ["gamma failed"]
    );
    assert_eq!(
        only(Filter {
            text: Some("BETA".to_owned()),
            ..Filter::default()
        }),
        ["beta failed"]
    );
    set_status(&c, &IssueIds::one(2), &IssueStatus::Ignored)
        .unwrap()
        .unwrap();
    assert_eq!(
        only(Filter {
            status: Some(Status::Ignored),
            ..Filter::default()
        }),
        ["beta failed"]
    );
    assert_eq!(
        only(Filter {
            project: Some(1),
            status: Some(Status::Unresolved),
            text: Some("failed".to_owned()),
            ..Filter::default()
        }),
        ["alpha failed", "gamma failed"]
    );
}

#[test]
fn a_cursor_pages_through_issues_with_equal_keys() {
    let c = memory();
    for n in 0..=PAGE {
        let fingerprint = json!({ "fingerprint": [format!("issue {n}")] });
        add(&c, 1, "boom", NOW, fingerprint);
    }
    let first = list(&c, &Filter::default(), Sort::Events, None, NOW).unwrap();
    let ids = first.issues.iter().map(|i| i.id).collect::<Vec<_>>();
    assert_eq!(ids, (2..=51).rev().collect::<Vec<i64>>());
    assert_eq!(first.next.as_deref(), Some("e1.2"));
    let after = Cursor::parse("e1.2").unwrap();
    assert_eq!(after.sort(), Sort::Events);
    let second = list(&c, &Filter::default(), Sort::Events, Some(after), NOW).unwrap();
    assert_eq!(second.issues.iter().map(|i| i.id).collect::<Vec<_>>(), [1]);
    assert_eq!(second.next, None);
}

#[test]
fn only_a_cursor_the_list_made_parses() {
    for bad in ["", "e", "e1", "x1.2", "e1.", "e.2", "e1.2.3", "eone.2"] {
        assert_eq!(Cursor::parse(bad), None, "{bad}");
    }
    let cursor = Cursor::parse("f-5.7").unwrap();
    assert_eq!(
        (cursor.sort(), cursor.encode()),
        (Sort::FirstSeen, "f-5.7".to_owned())
    );
}

fn status_of(c: &Connection, id: i64) -> (String, Option<String>, Option<i64>) {
    c.query_row(
        "SELECT status,resolved_in_release,resolved_at FROM issues WHERE id=?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )
    .unwrap()
}

#[test]
fn a_status_change_applies_to_all_issues_or_to_none() {
    let c = three_issues();
    let ids = IssueIds::new(vec![3, 1, 3]).unwrap();
    let resolved = IssueStatus::Resolved {
        in_release: Some("v3".to_owned()),
        at: 9000,
    };
    assert_eq!(set_status(&c, &ids, &resolved).unwrap(), Ok(()));
    for id in [1, 3] {
        assert_eq!(
            status_of(&c, id),
            ("resolved".to_owned(), Some("v3".to_owned()), Some(9000))
        );
    }
    assert_eq!(status_of(&c, 2), ("unresolved".to_owned(), None, None));
    let with_missing = IssueIds::new(vec![1, 2, 99]).unwrap();
    assert_eq!(
        set_status(&c, &with_missing, &IssueStatus::Ignored).unwrap(),
        Err("no issue 99".to_owned())
    );
    assert_eq!(status_of(&c, 2), ("unresolved".to_owned(), None, None));
    let reopen = IssueIds::new(vec![1, 2]).unwrap();
    assert_eq!(
        set_status(&c, &reopen, &IssueStatus::Unresolved).unwrap(),
        Ok(())
    );
    assert_eq!(status_of(&c, 1), ("unresolved".to_owned(), None, None));
    assert_eq!(status_of(&c, 3).0, "resolved");
}

#[test]
fn a_status_change_names_one_to_five_hundred_issues() {
    assert_eq!(
        IssueIds::new(Vec::new()),
        Err("name at least one issue".to_owned())
    );
    assert_eq!(
        IssueIds::new((1..=501).collect()),
        Err("a change names at most 500 issues, this one 501".to_owned())
    );
    assert!(IssueIds::new((1..=500).chain([500]).collect()).is_ok());
    assert_eq!(IssueIds::new(vec![7, 7]), Ok(IssueIds::one(7)));
}

#[test]
fn events_page_newest_first_and_the_newest_stored_one_has_a_payload() {
    let c = memory();
    for n in 0..=EVENT_PAGE {
        add(
            &c,
            1,
            "boom",
            NOW - H + i64::try_from(n).unwrap(),
            json!({}),
        );
    }
    let first = events(&c, 1, None).unwrap();
    assert_eq!(first.events.len(), EVENT_PAGE);
    assert_eq!(first.next.as_deref(), Some("2"));
    assert_eq!(first.events[0].timestamp, "2025-11-22T11:30:00.05Z");
    assert!(!first.events[0].stored, "the 51st event keeps no payload");
    assert!(first.events[1].stored);
    let second = events(&c, 1, Some(2)).unwrap();
    assert_eq!(second.events.len(), 1);
    assert_eq!(second.next, None);
    let (newest, blob) = stored_payload(&c, 1, None).unwrap().unwrap();
    assert_eq!(newest, first.events[1].event_id);
    let raw: Value = serde_json::from_slice(&zstd::decode_all(blob.as_slice()).unwrap()).unwrap();
    assert_eq!(raw["message"], "boom");
    let oldest = &second.events[0].event_id;
    assert_eq!(
        stored_payload(&c, 1, Some(oldest))
            .unwrap()
            .map(|(id, _)| id),
        Some(oldest.clone())
    );
    assert_eq!(
        stored_payload(&c, 1, Some(&first.events[0].event_id)).unwrap(),
        None
    );
    assert_eq!(stored_payload(&c, 2, None).unwrap(), None);
}
