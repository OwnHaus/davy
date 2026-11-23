use super::{Edit, edit};
use crate::{
    alert::{self, Destination},
    monitor::{
        CheckUrl, Expect, HeartbeatToken, NewMonitor, State, Target,
        checkin::MonitorSlug,
        checkin_store_tests::{memory, send},
        store::{self, Added},
    },
};
use rusqlite::Connection;
use std::time::Duration;

const HOUR: Duration = Duration::from_hours(1);

fn heartbeat(c: &Connection, project: i64, name: &str, slug: Option<&str>) -> Added {
    let target = Target::Heartbeat {
        grace: Duration::from_mins(5),
        slug: slug.map(|s| MonitorSlug::parse(s).unwrap()),
    };
    let m = NewMonitor::new(name, target, Some(HOUR)).unwrap();
    store::add(c, project, &m, 0).unwrap()
}

fn site(c: &Connection) -> Added {
    let target = Target::Http {
        url: CheckUrl::parse("https://shop.example/health").unwrap(),
        expect: Expect::Keyword("ok".into()),
        timeout: Duration::from_secs(10),
    };
    let m = NewMonitor::new("API", target, Some(Duration::from_mins(1))).unwrap();
    store::add(c, 1, &m, 0).unwrap()
}

fn token(added: &Added) -> HeartbeatToken {
    HeartbeatToken::parse(added.target.strip_prefix("/api/heartbeat/").unwrap()).unwrap()
}

/// Project, name, `every_s`, `grace_s`, slug and `due_at` of monitor `id`.
type Row = (
    String,
    String,
    i64,
    Option<i64>,
    Option<String>,
    Option<i64>,
);

fn row(c: &Connection, id: i64) -> Row {
    c.query_row(
        "SELECT p.slug,m.name,m.every_s,m.grace_s,m.slug,m.due_at FROM monitors m JOIN projects p ON p.id=m.project_id WHERE m.id=?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
    )
    .unwrap()
}

fn count(c: &Connection, sql: &str) -> i64 {
    c.query_row(sql, [], |r| r.get(0)).unwrap()
}

fn refused(c: &Connection, id: i64, e: &Edit) -> String {
    edit(c, id, e, 0).unwrap_err().to_string()
}

#[test]
fn an_edit_keeps_the_id_token_checks_and_incidents() {
    let c = memory();
    let added = heartbeat(&c, 1, "Backup", None);
    store::ping(&c, added.id, 1_000, false).unwrap();
    store::ping(&c, added.id, 2_000, true).unwrap();
    let change = Edit {
        name: Some(" Nightly backup ".into()),
        every: Some(2 * HOUR),
        ..Edit::default()
    };
    assert_eq!(edit(&c, added.id, &change, 3_000).unwrap(), added);
    assert_eq!(store::by_token(&c, &token(&added)).unwrap(), Some(added.id));
    assert_eq!(
        count(&c, "SELECT count(*) FROM checks WHERE monitor_id=1"),
        2
    );
    let listed = store::one(&c, added.id).unwrap().unwrap();
    assert_eq!(
        (listed.name.as_str(), listed.state, listed.open_since),
        ("Nightly backup", State::Down, Some(2_000))
    );
    assert_eq!(listed.schedule.as_deref(), Some("every 2h"));
    store::ping(&c, added.id, 4_000, false).unwrap();
    let incidents: Vec<(i64, Option<i64>)> = c
        .prepare("SELECT started_at,resolved_at FROM incidents")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(incidents, [(2_000, Some(4_000))]);
}

#[test]
fn a_heartbeat_that_expects_a_ping_expects_it_by_its_new_every_and_grace() {
    let c = memory();
    let pinged = heartbeat(&c, 1, "Backup", None).id;
    let never = heartbeat(&c, 1, "Report", None).id;
    store::ping(&c, pinged, 1_000, false).unwrap();
    assert_eq!(row(&c, pinged).5, Some(3_901_000));
    let every = Edit {
        every: Some(2 * HOUR),
        ..Edit::default()
    };
    edit(&c, pinged, &every, 5_000).unwrap();
    assert_eq!(row(&c, pinged).5, Some(7_501_000));
    let grace = Edit {
        grace: Some(Duration::ZERO),
        ..Edit::default()
    };
    edit(&c, pinged, &grace, 5_000).unwrap();
    assert_eq!(row(&c, pinged).5, Some(7_201_000));
    edit(&c, never, &every, 5_000).unwrap();
    assert_eq!(row(&c, never).5, None);
    store::sweep(&c, 7_201_001).unwrap();
    edit(&c, pinged, &grace, 7_300_000).unwrap();
    assert_eq!(
        row(&c, pinged),
        ("shop".into(), "Backup".into(), 7_200, Some(0), None, None)
    );
}

#[test]
fn an_http_check_takes_its_new_url_expectation_and_timeout() {
    let c = memory();
    let id = site(&c).id;
    let change = Edit {
        url: Some(CheckUrl::parse("https://shop.example/ready").unwrap()),
        expect: Some(Expect::Status),
        timeout: Some(Duration::from_secs(20)),
        ..Edit::default()
    };
    let edited = edit(&c, id, &change, 0).unwrap();
    assert_eq!(edited.target, "https://shop.example/ready");
    let due = store::due(&c, 0).unwrap();
    assert_eq!(
        (due[0].url.as_str(), &due[0].expect, due[0].timeout),
        (
            "https://shop.example/ready",
            &Expect::Status,
            Duration::from_secs(20)
        )
    );
}

#[test]
fn an_edit_is_held_to_the_limits_of_add() {
    let c = memory();
    let id = site(&c).id;
    let cases = [
        (
            Edit {
                timeout: Some(Duration::from_mins(1)),
                ..Edit::default()
            },
            "the timeout must be 1s to 60s and shorter than the interval",
        ),
        (
            Edit {
                every: Some(Duration::from_secs(10)),
                ..Edit::default()
            },
            "an HTTP check runs every 30s to 24h",
        ),
        (
            Edit {
                name: Some("  ".into()),
                ..Edit::default()
            },
            "the name must be 1 to 100 characters",
        ),
        (
            Edit {
                expect: Some(Expect::NoKeyword(String::new())),
                ..Edit::default()
            },
            "the keyword must be 1 to 1000 bytes",
        ),
    ];
    for (change, error) in cases {
        assert_eq!(refused(&c, id, &change), error, "{change:?}");
    }
    assert_eq!(refused(&c, 99, &Edit::default()), "no monitor 99");
}

#[test]
fn a_monitor_keeps_its_kind() {
    let c = memory();
    let http = site(&c).id;
    let beat = heartbeat(&c, 1, "Backup", None).id;
    let to_heartbeat = [
        Edit {
            grace: Some(Duration::ZERO),
            ..Edit::default()
        },
        Edit {
            slug: Some(None),
            ..Edit::default()
        },
    ];
    for change in to_heartbeat {
        assert_eq!(
            refused(&c, http, &change),
            "monitor 1 is an HTTP check, so it has no grace period or slug. \
             A monitor keeps its kind: add a heartbeat and remove this one"
        );
    }
    let to_http = [
        Edit {
            url: Some(CheckUrl::parse("https://shop.example/").unwrap()),
            ..Edit::default()
        },
        Edit {
            expect: Some(Expect::Status),
            ..Edit::default()
        },
        Edit {
            timeout: Some(Duration::from_secs(5)),
            ..Edit::default()
        },
    ];
    for change in to_http {
        assert_eq!(
            refused(&c, beat, &change),
            "monitor 2 is a heartbeat, so it has no URL, keyword or timeout. \
             A monitor keeps its kind: add an HTTP check and remove this one"
        );
    }
    assert_eq!(row(&c, beat).2, 3_600);
}

#[test]
fn a_monitor_made_by_check_ins_only_takes_a_new_name() {
    let c = memory();
    crate::store::create_project(&c, "blog").unwrap();
    let config = r#","monitor_config":{"schedule":{"type":"interval","value":30,"unit":"minute"}}"#;
    send(&c, "2026-11-20T08:00:00Z", "sync", "ok", config);
    let id = count(&c, "SELECT id FROM monitors WHERE slug='sync'");
    let refusals = [
        Edit {
            project: Some(2),
            ..Edit::default()
        },
        Edit {
            every: Some(HOUR),
            ..Edit::default()
        },
        Edit {
            grace: Some(Duration::ZERO),
            ..Edit::default()
        },
        Edit {
            slug: Some(Some(MonitorSlug::parse("other").unwrap())),
            ..Edit::default()
        },
        Edit {
            slug: Some(None),
            name: Some("Sync".into()),
            ..Edit::default()
        },
    ];
    for change in refusals {
        assert_eq!(
            refused(&c, id, &change),
            "monitor 1 was made by check-ins, which set its schedule and find it by project \
             and slug, so only its name can change",
            "{change:?}"
        );
    }
    let rename = Edit {
        name: Some("Billing sync".into()),
        ..Edit::default()
    };
    let edited = edit(&c, id, &rename, 0).unwrap();
    assert_eq!(edited.target, "check-in sync");
    assert_eq!(
        row(&c, id),
        (
            "shop".into(),
            "Billing sync".into(),
            1_800,
            Some(60),
            Some("sync".into()),
            Some(1_795_163_460_000)
        )
    );
}

/// The channel and type of every queued notice, oldest first.
fn notices(c: &Connection) -> Vec<(i64, String)> {
    c.prepare("SELECT channel_id,type FROM notifications ORDER BY id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

#[test]
fn a_moved_monitor_brings_its_open_incident_to_the_new_project() {
    let c = memory();
    crate::store::create_project(&c, "blog").unwrap();
    for project in [Some(1), Some(2), None] {
        let hook = Destination::Webhook(CheckUrl::parse("https://hooks.example/").unwrap());
        alert::store::add(&c, project, &hook, false, 0).unwrap();
    }
    let added = heartbeat(&c, 1, "Backup", None);
    store::ping(&c, added.id, 1_000, true).unwrap();
    let change = Edit {
        project: Some(2),
        ..Edit::default()
    };
    assert_eq!(edit(&c, added.id, &change, 2_000).unwrap(), added);
    let carried: String = c
        .query_row(
            "SELECT payload FROM notifications WHERE channel_id=2",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        carried,
        r#"{"id":3,"type":"monitor.down","project":"blog","at":"1970-01-01T00:00:02Z","monitor":{"id":1,"name":"Backup"},"incident":{"id":1,"reason":"failure ping"}}"#
    );
    store::ping(&c, added.id, 3_000, false).unwrap();
    assert_eq!(
        notices(&c),
        [
            (1, "monitor.down".into()),
            (3, "monitor.down".into()),
            (2, "monitor.down".into()),
            (2, "monitor.up".into()),
            (3, "monitor.up".into()),
        ]
    );
    assert_eq!(row(&c, added.id).0, "blog");
    assert_eq!(
        count(&c, "SELECT count(*) FROM checks WHERE monitor_id=1"),
        2
    );
}

#[test]
fn a_monitor_moved_without_an_incident_queues_nothing() {
    let c = memory();
    crate::store::create_project(&c, "blog").unwrap();
    let hook = Destination::Webhook(CheckUrl::parse("https://hooks.example/").unwrap());
    alert::store::add(&c, Some(2), &hook, false, 0).unwrap();
    let id = heartbeat(&c, 1, "Backup", None).id;
    store::ping(&c, id, 1_000, false).unwrap();
    let change = Edit {
        project: Some(2),
        ..Edit::default()
    };
    edit(&c, id, &change, 2_000).unwrap();
    assert_eq!(notices(&c), []);
}

#[test]
fn a_slug_stays_unique_in_the_project_it_moves_to() {
    let c = memory();
    crate::store::create_project(&c, "blog").unwrap();
    let moving = heartbeat(&c, 1, "Backup", Some("backup")).id;
    heartbeat(&c, 1, "Report", Some("report"));
    heartbeat(&c, 2, "Backup", Some("backup"));
    let to_blog = Edit {
        project: Some(2),
        ..Edit::default()
    };
    assert_eq!(
        refused(&c, moving, &to_blog),
        "the project already has a monitor with slug backup"
    );
    let taken = Edit {
        slug: Some(Some(MonitorSlug::parse("report").unwrap())),
        ..Edit::default()
    };
    assert_eq!(
        refused(&c, moving, &taken),
        "the project already has a monitor with slug report"
    );
    let same = Edit {
        slug: Some(Some(MonitorSlug::parse("backup").unwrap())),
        name: Some("Backups".into()),
        ..Edit::default()
    };
    edit(&c, moving, &same, 0).unwrap();
    let without = Edit {
        project: Some(2),
        slug: Some(None),
        ..Edit::default()
    };
    edit(&c, moving, &without, 0).unwrap();
    assert_eq!(
        row(&c, moving),
        (
            "blog".into(),
            "Backups".into(),
            3_600,
            Some(300),
            None,
            None
        )
    );
}
