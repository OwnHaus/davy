use crate::{envelope::*, event::*, grouping::*, store, test_support::fixture};
use serde_json::json;
#[test]
fn fixtures() {
    for sdk in ["sentry-node-7", "sentry-node-10"] {
        for n in 1..=4 {
            let body = fixture(&format!("{sdk}/{n}.body"));
            let e = Envelope::parse(&body).unwrap();
            let kinds: Vec<_> = e.items.iter().map(|i| &i.kind).collect();
            assert_eq!(
                kinds,
                if n == 1 || n == 3 {
                    vec![&ItemKind::Session]
                } else {
                    vec![&ItemKind::Event]
                }
            );
            if n == 4 {
                let event =
                    ErrorEvent::parse(serde_json::from_slice(e.items[0].payload).unwrap()).unwrap();
                assert_eq!(event.exceptions.len(), 1);
                assert_eq!(event.exceptions[0].ty, "SyntaxError");
                assert_eq!(
                    event.exceptions[0].value.as_deref(),
                    Some("Expected property name or '}' in JSON at position 1 (line 1 column 2)")
                );
            }
        }
    }
}
#[test]
fn fixture_transactions_are_dropped() {
    for n in [5, 6] {
        let body = fixture(&format!("sentry-node-10/{n}.body"));
        let e = Envelope::parse(&body).unwrap();
        assert_eq!(e.items.len(), 1);
        assert_eq!(e.items[0].kind, ItemKind::Transaction);
        assert_eq!(disposition(&e.items[0].kind), Disposition::Drop);
    }
}
fn event(v: serde_json::Value) -> ErrorEvent {
    ErrorEvent::parse(v).unwrap()
}
#[test]
fn grouping_rules() {
    let a = group(&event(
        json!({"message":"job 123 550e8400-e29b-41d4-a716-446655440000"}),
    ));
    let b = group(&event(
        json!({"message":"job 999 123e4567-e89b-12d3-a456-426614174000"}),
    ));
    assert_eq!(a.version, 1);
    assert_eq!(a.fingerprint, b.fingerprint);
    assert_eq!(a.fingerprint.len(), 32);
    let ex = |ty, line| {
        event(
            json!({"exception":{"values":[{"type":ty,"value":"oops", "stacktrace":{"frames":[{"filename":"main.3f9a1c2b.js?x=1","function":"crash","lineno":line,"in_app":true}]}}]}}),
        )
    };
    let a = group(&ex("TypeError", 1));
    assert_eq!(a.fingerprint, group(&ex("TypeError", 99)).fingerprint);
    assert_ne!(a.fingerprint, group(&ex("SyntaxError", 1)).fingerprint);
    assert_eq!(a.title, "TypeError: oops");
    let bare = event(json!({"exception":{"values":[{"type":"TypeError"}]}}));
    assert_eq!(group(&bare).title, "TypeError", "no value, no colon");
    assert_eq!(a.culprit.as_deref(), Some("crash"));
    let mut e = ex("TypeError", 1);
    e.fingerprint = Some(vec!["{{ default }}".into(), "x".into()]);
    assert_ne!(a.fingerprint, group(&e).fingerprint);
    assert_eq!(
        normalize("id 3 abcdef0123 a@example.com 10.0.0.1"),
        "id <number> <hex> <email> <ip>"
    );
    let a = event(json!({"logentry":{"message":"job %s","formatted":"job a"}}));
    let b = event(json!({"logentry":{"message":"job %s","formatted":"job b"}}));
    assert_eq!(group(&a).fingerprint, group(&b).fingerprint);
}
#[test]
fn a_repeated_default_in_a_fingerprint_counts_once() {
    let mut e = event(json!({"message": "x".repeat(100_000)}));
    let fingerprint = |defaults: usize| {
        let mut parts = vec!["{{ default }}".to_owned(); defaults];
        parts.push("checkout".into());
        Some(parts)
    };
    e.fingerprint = fingerprint(1);
    let once = group(&e).fingerprint;
    e.fingerprint = fingerprint(1_000);
    assert_eq!(group(&e).fingerprint, once);
}
#[test]
fn an_envelope_holds_at_most_1000_items() {
    let envelope = |n: usize| format!("{{}}\n{}", "{\"type\":\"session\"}\n{}\n".repeat(n));
    let most = envelope(MAX_ITEMS);
    assert_eq!(Envelope::parse(most.as_bytes()).unwrap().items.len(), 1000);
    let over = envelope(MAX_ITEMS + 1);
    let e = Envelope::parse(over.as_bytes()).err().unwrap();
    assert_eq!(e.to_string(), "more than 1000 items");
}
#[test]
fn envelope_and_item_headers_are_objects() {
    for body in [&b"[]\n"[..], b"\"x\"\n", b"{}\n[\"event\", 2]\n{}"] {
        assert!(Envelope::parse(body).is_err(), "{body:?}");
    }
    let long = format!(
        "{{\"sdk\":[{}{{}}],\"dsn\":\"d\"}}\n",
        "{},".repeat(100_000)
    );
    let header = EnvelopeHeader::parse(long.as_bytes()).unwrap();
    assert_eq!(
        header.dsn.as_deref(),
        Some("d"),
        "fields Davy does not read are skipped"
    );
}
#[test]
fn envelope_binary_and_policy() {
    let e = Envelope::parse(
        b"{}\n{\"type\":\"attachment\",\"length\":3}\na\nb\n{\"type\":\"future\"}\nx",
    )
    .unwrap();
    assert_eq!(e.items[0].payload, b"a\nb");
    assert_eq!(e.items[1].kind, ItemKind::Other);
    assert_eq!(disposition(&e.items[1].kind), Disposition::Drop);
    assert!(Envelope::parse(b"{}\n{\"type\":\"event\",\"length\":100}\nx").is_err());
    let e = Envelope::parse(b"{}\n\n{\"type\":\"session\"}\n{}\r\n\n{\"type\":\"event\"}\n{}\n\n")
        .unwrap();
    let kinds: Vec<_> = e.items.iter().map(|i| &i.kind).collect();
    assert_eq!(kinds, vec![&ItemKind::Session, &ItemKind::Event]);
    let e = Envelope::parse(b"{\"sent_at\":1700000000}\n{\"type\":\"event\"}\n{}").unwrap();
    assert_eq!(
        e.items.len(),
        1,
        "a header field Davy does not read cannot reject"
    );
}
#[test]
fn event_boundary() {
    let e = event(
        json!({"event_id":"550E8400-E29B-41D4-A716-446655440000","timestamp":"2020-01-01T00:00:00Z","message":{"formatted":"hi","message":"template"},"exception":[{"type":"Error"}]}),
    );
    assert_eq!(e.event_id.0, "550e8400e29b41d4a716446655440000");
    assert_eq!(e.timestamp, 1_577_836_800_000);
    assert_eq!(e.message.as_deref(), Some("hi"));
    assert_eq!(e.message_template.as_deref(), Some("template"));
    assert!(ErrorEvent::parse(json!({"event_id":"bad"})).is_err());
    let e = event(
        json!({"level":"warn","timestamp":"2020-01-01T00:00:00","fingerprint":["a",1,null],"exception":{"values":[{"type":"E","value":7,"stacktrace":{"frames":[{"function":"f","lineno":-1}]}}]}}),
    );
    assert_eq!(e.level, Level::Warning);
    assert_eq!(e.timestamp, 1_577_836_800_000);
    assert_eq!(e.fingerprint, Some(vec!["a".to_owned(), "1".to_owned()]));
    assert_eq!(e.exceptions[0].value, None);
    assert_eq!(e.exceptions[0].frames[0].function.as_deref(), Some("f"));
    let e = event(json!({"timestamp":"yesterday","level":7}));
    assert_eq!(e.level, Level::Error);
    assert!(e.timestamp > 1_577_836_800_000);
}
#[test]
fn an_event_keeps_only_a_trace_id_of_32_hex_digits() {
    let trace = |id: &str| event(json!({"contexts":{"trace":{"trace_id":id}}})).trace_id;
    assert_eq!(
        trace("0AF7651916CD43DD8448EB211C80319C").as_deref(),
        Some("0af7651916cd43dd8448eb211c80319c")
    );
    for bad in [
        "x OR level:error",
        "0af7651916cd43dd8448eb211c80319",
        "00000000000000000000000000000000",
    ] {
        assert_eq!(trace(bad), None, "{bad}");
    }
}
fn resolve(
    c: &rusqlite::Connection,
    issue: i64,
    release: Option<&str>,
    at: i64,
) -> Result<(), String> {
    let status = store::IssueStatus::Resolved {
        in_release: release.map(str::to_owned),
        at,
    };
    crate::issues::set_status(c, &crate::issues::IssueIds::one(issue), &status).unwrap()
}
fn memory() -> rusqlite::Connection {
    let mut c = rusqlite::Connection::open_in_memory().unwrap();
    store::init(&mut c).unwrap();
    c
}
#[test]
fn creating_a_project_twice_returns_the_same_key() {
    let c = memory();
    let first = store::create_project(&c, "shop").unwrap();
    let other = store::create_project(&c, "blog").unwrap();
    assert_eq!(store::create_project(&c, "shop").unwrap(), first);
    assert_ne!(other, first);
    assert_eq!(first.1.len(), 32);
}

#[test]
fn a_resolved_issue_reopens_only_for_a_release_that_can_contain_the_bug() {
    use store::IssueStatus::{Ignored, Resolved, Unresolved};
    let fixed_in = |release: Option<&str>| Resolved {
        in_release: release.map(str::to_owned),
        at: 100,
    };
    let cases = [
        (fixed_in(Some("v1")), None, Some(500)),
        (fixed_in(Some("v1")), Some(("v1", 50)), Some(500)),
        (fixed_in(Some("v1")), Some(("v0", 50)), None),
        (fixed_in(Some("v1")), Some(("v2", 150)), Some(500)),
        (fixed_in(None), Some(("v0", 50)), None),
        (fixed_in(None), Some(("v2", 150)), Some(500)),
        (Unresolved, None, None),
        (Ignored, Some(("v2", 150)), None),
    ];
    for (mut status, release, reopened) in cases {
        assert_eq!(status.on_event(release, 500), reopened, "{release:?}");
    }
}
#[test]
fn resolving_waits_for_the_next_release() {
    let c = memory();
    store::create_project(&c, "releases").unwrap();
    let in_release = |release: &str, received_at| ErrorEvent {
        received_at,
        ..event(json!({"message":"boom","release":release}))
    };
    let status = || {
        c.query_row("SELECT status FROM issues", [], |r| r.get::<_, String>(0))
            .unwrap()
    };
    store::insert(&c, 1, &in_release("v1", 1000)).unwrap();
    assert_eq!(resolve(&c, 1, None, 2000), Ok(()));
    store::insert(&c, 1, &in_release("v1", 3000)).unwrap();
    assert_eq!(status(), "resolved");
    store::insert(&c, 1, &in_release("v2", 3000)).unwrap();
    assert_eq!(status(), "unresolved");
    assert_eq!(resolve(&c, 99, None, 0), Err("no issue 99".to_owned()));
}
#[test]
fn a_new_and_a_regressed_issue_each_queue_one_notice() {
    let c = memory();
    store::create_project(&c, "shop").unwrap();
    let hook = crate::monitor::CheckUrl::parse("https://hooks.example/x").unwrap();
    crate::alert::store::add(&c, None, &crate::alert::Destination::Slack(hook), false, 0).unwrap();
    let in_release = |release: &str, received_at| ErrorEvent {
        received_at,
        ..event(json!({"message":"boom","release":release}))
    };
    store::insert(&c, 1, &in_release("v1", 1000)).unwrap();
    store::insert(&c, 1, &in_release("v1", 1500)).unwrap();
    assert_eq!(resolve(&c, 1, Some("v1"), 2000), Ok(()));
    store::insert(&c, 1, &in_release("v2", 3000)).unwrap();
    store::insert(&c, 1, &in_release("v2", 4000)).unwrap();
    let payloads: Vec<String> = c
        .prepare("SELECT payload FROM notifications ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        payloads,
        [
            r#"{"text":"[shop] New issue: boom","unfurl_links":false,"unfurl_media":false}"#,
            r#"{"text":"[shop] Issue regressed: boom","unfurl_links":false,"unfurl_media":false}"#,
        ]
    );
}
#[test]
fn migration_backfills_releases_from_old_events() {
    let mut c = memory();
    store::create_project(&c, "old").unwrap();
    let e = event(json!({"message":"boom","release":"v1"}));
    store::insert(&c, 1, &e).unwrap();
    c.execute_batch(&format!(
        "{tokens} {quota} {drops} {} {} DROP TRIGGER events_tags; DROP TRIGGER issues_text_insert; DROP TRIGGER issues_text_update; DROP TRIGGER issues_text_delete; DROP TABLE issue_tags; DROP TABLE issues_text; DROP INDEX issues_status_sorts; DROP INDEX issues_project_sorts; DROP TABLE releases; DROP TABLE checkin_starts; DROP TABLE checks; DROP TABLE incidents; DROP TABLE monitors; DROP TABLE notifications; DROP TABLE channels; DROP TABLE feedback; DROP TABLE sessions; DROP TABLE invites; DROP TABLE users; DROP TABLE firsts; DROP TABLE client_reports; DROP TABLE user_feedback; DROP TABLE release_sessions; DROP TABLE release_users; DROP TABLE session_seen; ALTER TABLE projects DROP COLUMN scrub; DROP TABLE latest_payloads; DROP TABLE log_days; DROP TABLE store_days; ALTER TABLE projects DROP COLUMN vercel_secret; ALTER TABLE issues DROP COLUMN discarded_at; DROP TABLE discarded_daily; DROP TABLE searches; DROP TABLE search_days; DROP INDEX projects_own; ALTER TABLE projects DROP COLUMN own; PRAGMA user_version=0;",
        crate::usage::UNDO,
        crate::retention::UNDO,
        drops = crate::drops::UNDO,
        tokens = crate::tokens::UNDO,
        quota = crate::quota::UNDO
    ))
    .unwrap();
    store::init(&mut c).unwrap();
    store::init(&mut c).unwrap();
    let row: (String, i64) = c
        .query_row("SELECT version,first_seen FROM releases", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(row, ("v1".into(), e.received_at));
    let monitors: i64 = c
        .query_row("SELECT count(*) FROM monitors", [], |r| r.get(0))
        .unwrap();
    assert_eq!(monitors, 0);
    let channels: i64 = c
        .query_row("SELECT count(*) FROM channels", [], |r| r.get(0))
        .unwrap();
    assert_eq!(channels, 0);
    let users: i64 = c
        .query_row("SELECT count(*) FROM users", [], |r| r.get(0))
        .unwrap();
    assert_eq!(users, 0);
    let version: i64 = c
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, i64::from(store::SCHEMA_VERSION));
}
#[test]
fn migration_indexes_the_text_and_tags_of_old_issues() {
    let mut c = memory();
    store::create_project(&c, "old").unwrap();
    store::insert(
        &c,
        1,
        &event(json!({"message":"Checkout Timed Out","environment":"prod","release":"v1"})),
    )
    .unwrap();
    c.execute_batch(&format!(
        "{tokens} {quota} {drops} {} {} DROP TRIGGER events_tags; DROP TRIGGER issues_text_insert; DROP TRIGGER issues_text_update; DROP TRIGGER issues_text_delete; DROP TABLE issue_tags; DROP TABLE issues_text; DROP INDEX issues_status_sorts; DROP INDEX issues_project_sorts; DROP TABLE firsts; DROP TABLE client_reports; DROP TABLE user_feedback; DROP TABLE release_sessions; DROP TABLE release_users; DROP TABLE session_seen; ALTER TABLE notifications DROP COLUMN issue; ALTER TABLE channels DROP COLUMN sdk_dropped; {} ALTER TABLE projects DROP COLUMN scrub; DROP TABLE latest_payloads; DROP TABLE log_days; DROP TABLE store_days; ALTER TABLE projects DROP COLUMN vercel_secret; ALTER TABLE issues DROP COLUMN discarded_at; DROP TABLE discarded_daily; DROP TABLE searches; DROP TABLE search_days; DROP INDEX projects_own; ALTER TABLE projects DROP COLUMN own; PRAGMA user_version=6;",
        crate::usage::UNDO,
        crate::retention::UNDO,
        crate::monitor::checkin_store::UNDO,
        drops = crate::drops::UNDO,
        tokens = crate::tokens::UNDO,
        quota = crate::quota::UNDO
    ))
    .unwrap();
    store::init(&mut c).unwrap();
    let tags: Vec<(String, String, i64)> = c
        .prepare("SELECT key,value,issue_id FROM issue_tags")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        tags,
        [
            ("environment".into(), "prod".into(), 1),
            ("release".into(), "v1".into(), 1)
        ]
    );
    let found: i64 = c
        .query_row(
            "SELECT rowid FROM issues_text WHERE issues_text MATCH '\"timed out\"'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(found, 1);
    store::insert(
        &c,
        1,
        &event(json!({"message":"Checkout Timed Out","environment":"staging"})),
    )
    .unwrap();
    let staging: i64 = c
        .query_row(
            "SELECT issue_id FROM issue_tags WHERE key='environment' AND value='staging'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(staging, 1);
}
#[test]
fn storage_regression_duplicates_sampling_and_ignored() {
    let mut c = memory();
    store::create_project(&c, "storage").unwrap();
    let first = event(json!({"message":"failure 1"}));
    {
        let tx = c.transaction().unwrap();
        store::insert(&tx, 1, &first).unwrap();
        tx.commit().unwrap();
    }
    c.execute(
        "UPDATE issues SET status='resolved',resolved_at=1,resolved_in_release='v1'",
        [],
    )
    .unwrap();
    store::insert(&c, 1, &first).unwrap();
    assert_eq!(
        c.query_row("SELECT status FROM issues", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "resolved"
    );
    let next = event(json!({"message":"failure 2"}));
    store::insert(&c, 1, &next).unwrap();
    let state: (String, i64, Option<i64>) = c
        .query_row(
            "SELECT status,regressed_at,resolved_at FROM issues",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(state, ("unresolved".into(), next.received_at, None));
    c.execute("UPDATE issues SET status='ignored'", []).unwrap();
    for _ in 2..101 {
        store::insert(&c, 1, &event(json!({"message":"failure 3"}))).unwrap();
    }
    assert_eq!(
        c.query_row("SELECT status,event_count FROM issues", [], |r| Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?
        )))
        .unwrap(),
        ("ignored".into(), 101)
    );
    assert_eq!(
        c.query_row("SELECT sum(count) FROM issue_hourly", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        101
    );
    let payloads = |c: &rusqlite::Connection| {
        c.query_row(
            "SELECT (SELECT count(payload) FROM events) + (SELECT count(*) FROM latest_payloads)",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap()
    };
    assert_eq!(payloads(&c), 52);
    store::insert(&c, 1, &event(json!({"message":"failure 4"}))).unwrap();
    assert_eq!(payloads(&c), 52);
    assert_eq!(
        store::payload(&c, 1).unwrap().unwrap()["message"],
        "failure 4"
    );
}
#[test]
fn grouping_frame_selection_and_bundle_names() {
    let make = |filename: &str, function: &str, in_app: bool| json!({"filename":filename,"function":function,"in_app":in_app});
    let mut frames = vec![
        make("vendor.js", "vendor", false),
        make("main.3f9a1c2b.js?build=1", "crash", true),
    ];
    let with_frames = |frames: &Vec<serde_json::Value>| {
        event(
            json!({"exception":{"values":[{"type":"Error","value":"failed","stacktrace":{"frames":frames}}]}}),
        )
    };
    let a = group(&with_frames(&frames));
    frames[0] = make("different.js", "different", false);
    frames[1] = make("main.abcdef01.js?build=99", "crash", true);
    assert_eq!(a.fingerprint, group(&with_frames(&frames)).fingerprint);
    let mut frames: Vec<_> = (0..11)
        .map(|i| make("app.js", &format!("frame{i}"), true))
        .collect();
    let a = group(&with_frames(&frames));
    frames[0] = make("other.js", "excluded", true);
    assert_eq!(a.fingerprint, group(&with_frames(&frames)).fingerprint);
    frames[10] = make("app.js", "changed", true);
    assert_ne!(a.fingerprint, group(&with_frames(&frames)).fingerprint);
}

mod http;

#[tokio::test(start_paused = true)]
async fn the_sweep_task_queues_a_sweep_every_period_until_the_writer_stops() {
    let (sender, inbox) = crossbeam_channel::bounded(8);
    let task = tokio::spawn(store::sweep_every(
        sender,
        std::time::Duration::from_secs(10),
        crate::retention::Defaults::default().errors,
    ));
    tokio::time::sleep(std::time::Duration::from_secs(25)).await;
    let sweeps = inbox
        .try_iter()
        .filter(|w| matches!(w.job, store::Job::Sweep { .. }))
        .count();
    assert_eq!(sweeps, 3);
    drop(inbox);
    tokio::time::sleep(std::time::Duration::from_secs(10)).await;
    assert!(task.is_finished());
}

#[cfg(unix)]
#[test]
fn a_new_data_directory_is_readable_by_its_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let root = crate::test_support::scratch("data-mode");
    let data = root.join("nested/data");
    store::open(&data).unwrap();
    let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&data), 0o700);
    std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o750)).unwrap();
    store::open(&data).unwrap();
    assert_eq!(mode(&data), 0o750, "an existing directory keeps its mode");
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn the_writer_keeps_the_payload_of_the_newest_event_of_each_batch() {
    let root = crate::test_support::scratch("writer-newest");
    let writer = store::Writer::start(store::open(&root).unwrap(), 8);
    let c = store::open(&root).unwrap();
    store::create_project(&c, "shop").unwrap();
    let ingest = |messages: std::ops::Range<i64>| {
        let events = messages
            .map(|n| event(json!({"message":"boom","extra":{"n":n}})))
            .collect();
        let (done, outcome) = tokio::sync::oneshot::channel();
        let held = crate::http::body::Budget::new(0).hold();
        let job = store::Job::Ingest {
            project: 1,
            events,
            held,
        };
        writer.sender.send(store::Write { job, done }).unwrap();
        outcome.blocking_recv().unwrap().unwrap();
    };
    let kept = || -> Vec<i64> {
        c.prepare("SELECT id FROM events WHERE payload IS NOT NULL OR id IN (SELECT event FROM latest_payloads) ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    ingest(0..70);
    assert_eq!(kept(), (1..=50).chain([70]).collect::<Vec<_>>());
    ingest(70..72);
    assert_eq!(kept(), (1..=50).chain([72]).collect::<Vec<_>>());
    assert_eq!(store::payload(&c, 1).unwrap().unwrap()["extra"]["n"], 71);
    std::fs::remove_dir_all(root).unwrap();
}
