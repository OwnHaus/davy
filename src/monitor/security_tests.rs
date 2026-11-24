use super::checkin_store_tests::{memory, ms, row, send};
use super::{NewMonitor, Target, checkin::MonitorSlug, store};
use std::time::Duration;

#[test]
fn starts_preserve_cli_expectation_and_open_run_deadline() {
    let c = memory();
    let m = store::add(
        &c,
        1,
        &NewMonitor::new(
            "daily",
            Target::Heartbeat {
                grace: Duration::ZERO,
                slug: MonitorSlug::parse("daily"),
            },
            Some(Duration::from_hours(24)),
        )
        .unwrap(),
        0,
    )
    .unwrap();
    store::ping(&c, m.id, 0, false).unwrap();
    send(&c, "2026-11-23T16:00:00Z", "daily", "in_progress", "");
    assert_eq!(
        row(&c, "daily").due_at.as_deref(),
        Some("1970-01-02T00:00:00Z")
    );
    send(
        &c,
        "2026-11-23T16:00:00Z",
        "job",
        "in_progress",
        r#","monitor_config":{"schedule":{"type":"interval","value":1,"unit":"hour"},"max_runtime":10}"#,
    );
    send(
        &c,
        "2026-11-23T16:01:00Z",
        "job",
        "in_progress",
        r#","monitor_config":{"schedule":{"type":"interval","value":1,"unit":"hour"},"max_runtime":60}"#,
    );
    assert_eq!(
        row(&c, "job").due_at.as_deref(),
        Some("2026-11-23T16:10:00Z")
    );
}

#[test]
fn starts_are_rate_limited() {
    let c = memory();
    for _ in 0..7 {
        send(
            &c,
            "2026-11-23T16:00:00Z",
            "job",
            "in_progress",
            r#","monitor_config":{"schedule":{"type":"interval","value":1,"unit":"hour"}}"#,
        );
    }
    let drops: i64 = c
        .query_row(
            "SELECT quantity FROM client_reports WHERE reason='davy_rate'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(drops, 1);
}

#[test]
fn config_changes_leave_a_count() {
    let c = memory();
    for value in [1, 1, 2] {
        send(
            &c,
            "2026-11-23T16:00:00Z",
            "job",
            "ok",
            &format!(
                r#","monitor_config":{{"schedule":{{"type":"interval","value":{value},"unit":"hour"}}}}"#
            ),
        );
    }
    let count: i64 = c
        .query_row("SELECT config_changes FROM monitors", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(store::list(&c).unwrap()[0].config_changes, 1);
}

#[test]
fn unreadable_schedule_does_not_break_listing_or_other_checkins() {
    let c = memory();
    send(
        &c,
        "2026-11-23T16:00:00Z",
        "bad",
        "ok",
        r#","monitor_config":{"schedule":{"type":"crontab","value":"0 * * * *"},"timezone":"UTC"}"#,
    );
    c.execute("UPDATE monitors SET zone='Missing/Zone'", [])
        .unwrap();
    assert_eq!(
        store::list(&c).unwrap()[0].schedule.as_deref(),
        Some("invalid stored schedule")
    );
    send(&c, "2026-11-23T16:01:00Z", "bad", "ok", "");
    assert_eq!(
        row(&c, "bad").due_at.as_deref(),
        Some("2026-11-23T17:01:00Z")
    );
}

#[test]
fn sweep_prunes_only_expired_resolved_history() {
    let c = memory();
    send(
        &c,
        "2026-11-23T16:00:00Z",
        "job",
        "error",
        r#","monitor_config":{"schedule":{"type":"interval","value":1,"unit":"hour"}}"#,
    );
    send(&c, "2026-11-23T16:01:00Z", "job", "ok", "");
    c.execute(
        "INSERT INTO incidents(monitor_id,started_at,reason) VALUES(1,0,'open')",
        [],
    )
    .unwrap();
    c.execute("INSERT INTO incidents(monitor_id,started_at,resolved_at,reason) SELECT 1,0,1,'old' FROM json_each(?1)",
        [serde_json::to_string(&vec![0;1001]).unwrap()]).unwrap();
    // 1,001 old incidents are two batches, and a sweep is sure of one.
    for _ in 0..2 {
        store::sweep(&c, ms("2026-12-01T00:00:00Z")).unwrap();
    }
    let count: i64 = c
        .query_row("SELECT count(*) FROM incidents", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 3);
}

#[test]
fn sdk_flapping_is_bounded_and_legitimate_alerts_pass() {
    use super::CheckUrl;
    use crate::alert::{self, Destination, EmailAddress, Issue, Notice};
    let c = memory();
    for destination in [
        Destination::Webhook(CheckUrl::parse("https://example.com/hooks").unwrap()),
        Destination::Slack(CheckUrl::parse("https://example.com/slack").unwrap()),
        Destination::Email(EmailAddress::parse("ops@example.com").unwrap()),
    ] {
        alert::store::add(&c, Some(1), &destination, false, 0).unwrap();
    }
    let cli = store::add(
        &c,
        1,
        &NewMonitor::new(
            "cli",
            Target::Heartbeat {
                grace: Duration::ZERO,
                slug: None,
            },
            None,
        )
        .unwrap(),
        0,
    )
    .unwrap();
    let mut sdk = [0; 3];
    let mut issues = [0; 3];
    let mut cli_notices = [0; 3];
    let url = url::Url::parse("https://davy.example/").unwrap();
    for second in 0..120 {
        let now = second * 1000;
        if second % 10 == 0 {
            for monitor in 0..100 {
                let status = if second % 20 == 0 { "error" } else { "ok" };
                let json = format!(
                    r#"{{"monitor_slug":"job-{monitor}","status":"{status}","monitor_config":{{"schedule":{{"type":"interval","value":1,"unit":"hour"}}}}}}"#
                );
                super::checkin_store::apply(&c, 1, now, &super::checkin::parse(json.as_bytes()))
                    .unwrap();
            }
        }
        if second == 15 {
            alert::store::notify(
                &c,
                1,
                &Notice::IssueNew(Issue {
                    id: 99,
                    title: "real".into(),
                    culprit: None,
                    level: "error".into(),
                    release: None,
                }),
                now,
            )
            .unwrap();
            store::ping(&c, cli.id, now, true).unwrap();
        }
        for due in alert::store::due(&c, now, &url).unwrap() {
            let index = usize::try_from(due.channel - 1).unwrap();
            let payload: serde_json::Value = serde_json::from_str(&due.payload).unwrap();
            if second == 15 {
                assert!(due.payload.contains("cli"));
                cli_notices[index] += 1;
            } else if second == 45 {
                assert!(due.payload.contains("real"));
                issues[index] += 1;
            } else {
                sdk[index] += 1;
                if index == 0 {
                    assert_eq!(payload["type"], "monitor.digest");
                    assert_eq!(payload["notices"].as_array().unwrap().len(), 100);
                }
            }
            let ids: Vec<_> = std::iter::once(due.id).chain(due.merged).collect();
            alert::store::delivered(&c, &ids, now, &Ok(())).unwrap();
        }
    }
    // At 30, 60 and 90 s: one digest per 30 s window, never two.
    assert_eq!(sdk, [3, 3, 3]);
    assert_eq!(issues, [1, 1, 1]);
    assert_eq!(cli_notices, [1, 1, 1]);
}

#[test]
fn completed_notifications_expire_but_pending_remain() {
    use crate::alert::{self, Destination, Notice};
    let c = memory();
    alert::store::add(
        &c,
        Some(1),
        &Destination::Webhook(super::CheckUrl::parse("https://example.com").unwrap()),
        false,
        0,
    )
    .unwrap();
    for t in 0..3 {
        alert::store::notify(&c, 1, &Notice::Test, t).unwrap();
    }
    alert::store::delivered(&c, &[1], 1, &Ok(())).unwrap();
    for _ in 0..6 {
        alert::store::delivered(&c, &[2], 2, &Err("down".into())).unwrap();
    }
    store::sweep(&c, ms("2026-12-01T00:00:00Z")).unwrap();
    let ids = c
        .prepare("SELECT id FROM notifications")
        .unwrap()
        .query_map([], |r| r.get::<_, i64>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(ids, [3]);
}

#[test]
fn a_waiting_issue_does_not_hold_back_the_sdk_digest() {
    use super::CheckUrl;
    use crate::alert::{self, Destination, Issue, Notice};
    let c = memory();
    let hook = Destination::Webhook(CheckUrl::parse("https://example.com/hooks").unwrap());
    alert::store::add(&c, Some(1), &hook, false, 0).unwrap();
    let json = r#"{"monitor_slug":"job","status":"error","monitor_config":{"schedule":{"type":"interval","value":1,"unit":"hour"}}}"#;
    super::checkin_store::apply(&c, 1, 0, &super::checkin::parse(json.as_bytes())).unwrap();
    let issue = Issue {
        id: 7,
        title: "boom".into(),
        culprit: None,
        level: "error".into(),
        release: None,
    };
    alert::store::notify(&c, 1, &Notice::IssueNew(issue), 25_000).unwrap();
    let url = url::Url::parse("https://davy.example/").unwrap();
    let types = |now| -> Vec<String> {
        alert::store::due(&c, now, &url)
            .unwrap()
            .iter()
            .map(|d| {
                serde_json::from_str::<serde_json::Value>(&d.payload).unwrap()["type"].to_string()
            })
            .collect()
    };
    assert_eq!(types(29_000), Vec::<String>::new());
    assert_eq!(types(30_000), [r#""monitor.digest""#]);
}

#[test]
fn a_run_left_open_on_a_down_monitor_expires() {
    let c = memory();
    let config = r#","monitor_config":{"schedule":{"type":"interval","value":1,"unit":"hour"},"max_runtime":10}"#;
    send(&c, "2026-11-23T16:00:00Z", "job", "error", config);
    send(&c, "2026-11-23T16:01:00Z", "job", "in_progress", "");
    let started = || -> Option<i64> {
        c.query_row(
            "SELECT run_started_at FROM monitors WHERE slug='job'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(started(), Some(ms("2026-11-23T16:01:00Z")));
    store::sweep(&c, ms("2026-11-23T16:10:00Z")).unwrap();
    assert_eq!(started(), Some(ms("2026-11-23T16:01:00Z")));
    store::sweep(&c, ms("2026-11-23T16:12:00Z")).unwrap();
    assert_eq!(started(), None);
    send(&c, "2026-11-23T16:13:00Z", "job", "in_progress", "");
    assert_eq!(started(), Some(ms("2026-11-23T16:13:00Z")));
}

#[test]
fn an_sdk_flood_counts_apart_from_operator_drops() {
    use crate::alert::{self, Destination, Notice};
    let c = memory();
    let hook = Destination::Webhook(super::CheckUrl::parse("https://example.com/hooks").unwrap());
    alert::store::add(&c, Some(1), &hook, false, 0).unwrap();
    let config = r#","monitor_config":{"schedule":{"type":"interval","value":1,"unit":"hour"}}"#;
    for monitor in 0..100 {
        send(
            &c,
            "2026-11-23T16:00:00Z",
            &format!("job-{monitor}"),
            "error",
            config,
        );
    }
    send(&c, "2026-11-23T16:01:00Z", "job-0", "ok", "");
    let counts = |c: &rusqlite::Connection| {
        let ch = &alert::store::list(c).unwrap()[0];
        (ch.unsent, ch.dropped, ch.sdk_dropped)
    };
    assert_eq!(counts(&c), (100, 0, 1));
    for at in 0..101 {
        alert::store::notify(&c, 1, &Notice::Test, at).unwrap();
    }
    assert_eq!(counts(&c), (200, 1, 1));
}
