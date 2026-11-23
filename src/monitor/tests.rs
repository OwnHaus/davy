use super::*;
use std::time::Duration;

#[test]
fn a_monitor_moves_through_its_states_on_each_outcome() {
    use {Change::*, Outcome::Up as Ok, State::*};
    let down = |r: Reason| Outcome::Down(r);
    let http = Reason::Status(503);
    let cases = [
        (Pending, Ok, Up, None),
        (Pending, down(http.clone()), Failing, None),
        (
            Pending,
            down(Reason::Late),
            Down,
            Some(Opened(Reason::Late)),
        ),
        (
            Pending,
            down(Reason::Failed),
            Down,
            Some(Opened(Reason::Failed)),
        ),
        (Up, Ok, Up, None),
        (Up, down(http.clone()), Failing, None),
        (Up, down(Reason::Late), Down, Some(Opened(Reason::Late))),
        (Up, down(Reason::Failed), Down, Some(Opened(Reason::Failed))),
        (Failing, Ok, Up, None),
        (
            Failing,
            down(http.clone()),
            Down,
            Some(Opened(Reason::Status(503))),
        ),
        (
            Failing,
            down(Reason::Late),
            Down,
            Some(Opened(Reason::Late)),
        ),
        (
            Failing,
            down(Reason::Failed),
            Down,
            Some(Opened(Reason::Failed)),
        ),
        (Down, Ok, Up, Some(Resolved)),
        (Down, down(http), Down, None),
        (Down, down(Reason::Late), Down, None),
        (Down, down(Reason::Failed), Down, None),
    ];
    for (from, outcome, to, change) in cases {
        let label = format!("{from:?} + {outcome:?}");
        assert_eq!(from.next(outcome), (to, change), "{label}");
    }
}

fn memory() -> rusqlite::Connection {
    let mut c = rusqlite::Connection::open_in_memory().unwrap();
    crate::store::init(&mut c).unwrap();
    crate::store::create_project(&c, "shop").unwrap();
    c
}

fn http(url: &str, expect: Expect) -> Target {
    Target::Http {
        url: CheckUrl::parse(url).unwrap(),
        expect,
        timeout: Duration::from_secs(10),
    }
}

#[test]
fn a_check_url_is_http_with_a_host_and_no_user_info() {
    assert_eq!(
        CheckUrl::parse("https://shop.example/health").map(|u| u.as_str().to_owned()),
        Ok("https://shop.example/health".to_owned())
    );
    for (bad, error) in [
        ("ftp://shop.example/", "the URL must be http or https"),
        (
            "https://user:pw@shop.example/",
            "the URL must not hold a user or password",
        ),
        (
            "https://user@shop.example/",
            "the URL must not hold a user or password",
        ),
        (
            "shop.example",
            "the URL is not valid: relative URL without a base",
        ),
        ("http://", "the URL is not valid: empty host"),
    ] {
        assert_eq!(CheckUrl::parse(bad), Err(error.to_owned()), "{bad}");
    }
    let long = format!("https://shop.example/{}", "a".repeat(2048));
    assert_eq!(
        CheckUrl::parse(&long),
        Err("the URL is longer than 2048 bytes".to_owned())
    );
}

/// The interval in seconds a new monitor gets, or why it is refused.
fn every(name: &str, target: Target, every: Option<u64>) -> Result<u64, String> {
    NewMonitor::new(name, target, every.map(Duration::from_secs)).map(|m| m.every.as_secs())
}

#[test]
fn an_http_monitor_is_held_to_its_limits() {
    let url = "https://shop.example/";
    let status = || http(url, Expect::Status);
    let timeout = |secs| Target::Http {
        url: CheckUrl::parse(url).unwrap(),
        expect: Expect::Status,
        timeout: Duration::from_secs(secs),
    };
    assert_eq!(every("API", status(), None), Ok(60));
    assert_eq!(every("API", timeout(1), Some(30)), Ok(30));
    let name = "the name must be 1 to 100 characters";
    let interval = "an HTTP check runs every 30s to 24h";
    let slow = "the timeout must be 1s to 60s and shorter than the interval";
    let keyword = "the keyword must be 1 to 1000 bytes";
    for (label, target, interval_secs, error) in [
        (" ", status(), None, name),
        (&*"n".repeat(101), status(), None, name),
        ("API", status(), Some(29), interval),
        ("API", status(), Some(86_401), interval),
        ("API", timeout(0), None, slow),
        ("API", timeout(61), Some(120), slow),
        ("API", timeout(30), Some(30), slow),
        (
            "API",
            http(url, Expect::Keyword(String::new())),
            None,
            keyword,
        ),
        (
            "API",
            http(url, Expect::NoKeyword("k".repeat(1001))),
            None,
            keyword,
        ),
    ] {
        let got = every(label, target, interval_secs);
        assert_eq!(got, Err(error.to_owned()), "{label} {interval_secs:?}");
    }
}

#[test]
fn a_heartbeat_monitor_is_held_to_its_limits() {
    let heartbeat = |grace| Target::Heartbeat {
        grace: Duration::from_secs(grace),
        slug: None,
    };
    assert_eq!(every("Backup", heartbeat(300), None), Ok(3600));
    assert_eq!(every("Backup", heartbeat(0), Some(60)), Ok(60));
    assert_eq!(
        every("Backup", heartbeat(86_400), Some(2_678_400)),
        Ok(2_678_400)
    );
    let interval = "a heartbeat expects a ping every 1m to 31d";
    for (grace, interval_secs, error) in [
        (300, Some(59), interval),
        (300, Some(2_678_401), interval),
        (86_401, None, "the grace period must be 0s to 24h"),
    ] {
        let got = every("Backup", heartbeat(grace), interval_secs);
        assert_eq!(got, Err(error.to_owned()), "{grace} {interval_secs:?}");
    }
}

#[test]
fn monitors_are_added_listed_and_removed() {
    let c = memory();
    let api = NewMonitor::new(
        "API",
        http("https://shop.example/health", Expect::Keyword("ok".into())),
        None,
    )
    .unwrap();
    let backup = NewMonitor::new(
        "Backup",
        Target::Heartbeat {
            grace: Duration::from_secs(300),
            slug: None,
        },
        None,
    )
    .unwrap();
    let first = store::add(&c, 1, &api, 1_000).unwrap();
    assert_eq!(
        first,
        store::Added {
            id: 1,
            target: "https://shop.example/health".into()
        }
    );
    let second = store::add(&c, 1, &backup, 2_000).unwrap();
    assert_eq!(second.id, 2);
    let token = second
        .target
        .strip_prefix("/api/heartbeat/")
        .and_then(HeartbeatToken::parse)
        .unwrap();
    assert_eq!(store::by_token(&c, &token).unwrap(), Some(2));
    let listed: Vec<_> = store::list(&c)
        .unwrap()
        .into_iter()
        .map(|m| {
            (
                m.id,
                m.project,
                m.name,
                m.target,
                m.state,
                m.last_check_at,
                m.open_since,
            )
        })
        .collect();
    assert_eq!(
        listed,
        [
            (
                1,
                "shop".to_owned(),
                "API".to_owned(),
                "https://shop.example/health".to_owned(),
                State::Pending,
                None,
                None
            ),
            (
                2,
                "shop".to_owned(),
                "Backup".to_owned(),
                format!("/api/heartbeat/{}", token.as_str()),
                State::Pending,
                None,
                None
            ),
        ]
    );
    assert!(store::remove(&c, 2).unwrap());
    assert!(!store::remove(&c, 2).unwrap());
    assert_eq!(store::by_token(&c, &token).unwrap(), None);
    assert_eq!(store::list(&c).unwrap().len(), 1);
}

#[test]
fn a_data_directory_holds_at_most_1000_monitors() {
    let c = memory();
    c.execute_batch(
        "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n LIMIT 1000)
         INSERT INTO monitors(project_id,name,kind,url,expect,every_s,timeout_s,state,created_at)
         SELECT 1,'m'||x,'http','https://shop.example/','status',60,10,'pending',0 FROM n",
    )
    .unwrap();
    let api = NewMonitor::new("API", http("https://shop.example/", Expect::Status), None).unwrap();
    assert_eq!(
        store::add(&c, 1, &api, 0).unwrap_err().to_string(),
        "a data directory holds at most 1000 monitors"
    );
}

#[test]
fn a_heartbeat_token_is_32_lowercase_hex_characters() {
    let good = "0123456789abcdef0123456789abcdef";
    assert_eq!(
        HeartbeatToken::parse(good).map(|t| t.as_str().to_owned()),
        Some(good.to_owned())
    );
    for bad in [
        "0123456789ABCDEF0123456789abcdef",
        "0123456789abcdef0123456789abcde",
        "0123456789abcdef0123456789abcdef0",
        "0123456789abcdef0123456789abcdeg",
        "",
    ] {
        assert!(HeartbeatToken::parse(bad).is_none(), "{bad}");
    }
}

#[test]
fn a_reason_reads_as_a_short_phrase() {
    for (reason, text) in [
        (Reason::Timeout, "timeout"),
        (
            Reason::Connect("connection refused".into()),
            "connection failed: connection refused",
        ),
        (Reason::Status(503), "HTTP 503"),
        (Reason::KeywordMissing, "keyword missing"),
        (Reason::KeywordFound, "keyword found"),
        (Reason::NotPublic, "address is not public"),
        (Reason::Late, "no ping in time"),
        (Reason::Failed, "failure ping"),
        (Reason::Missed, "missed check-in"),
        (Reason::TimedOut(30), "run timed out after 30 min"),
    ] {
        assert_eq!(reason.to_string(), text);
    }
}

fn backup(c: &rusqlite::Connection) -> i64 {
    let target = Target::Heartbeat {
        grace: Duration::from_secs(300),
        slug: None,
    };
    let m = NewMonitor::new("Backup", target, None).unwrap();
    store::add(c, 1, &m, 0).unwrap().id
}

/// State, last check, open incident start and last ping of monitor `id`.
fn status(c: &rusqlite::Connection, id: i64) -> (State, Option<i64>, Option<i64>, Option<i64>) {
    let m = store::list(c)
        .unwrap()
        .into_iter()
        .find(|m| m.id == id)
        .unwrap();
    let ping = c
        .query_row("SELECT last_ping_at FROM monitors WHERE id=?1", [id], |r| {
            r.get(0)
        })
        .unwrap();
    (m.state, m.last_check_at, m.open_since, ping)
}

#[test]
fn pings_move_a_heartbeat_and_open_and_close_its_incident() {
    let c = memory();
    let id = backup(&c);
    assert!(store::ping(&c, id, 1_000, false).unwrap());
    assert_eq!(status(&c, id), (State::Up, Some(1_000), None, Some(1_000)));
    assert!(store::ping(&c, id, 2_000, true).unwrap());
    assert_eq!(
        status(&c, id),
        (State::Down, Some(2_000), Some(2_000), Some(2_000))
    );
    assert!(store::ping(&c, id, 3_000, true).unwrap());
    assert!(store::ping(&c, id, 4_000, false).unwrap());
    assert_eq!(status(&c, id), (State::Up, Some(4_000), None, Some(4_000)));
    let incidents: Vec<(i64, Option<i64>, String)> = c
        .prepare("SELECT started_at,resolved_at,reason FROM incidents ORDER BY id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(incidents, [(2_000, Some(4_000), "failure ping".to_owned())]);
    let checks: Vec<(i64, bool, Option<String>)> = c
        .prepare("SELECT at,ok,reason FROM checks ORDER BY at")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let failed = Some("failure ping".to_owned());
    assert_eq!(
        checks,
        [
            (1_000, true, None),
            (2_000, false, failed.clone()),
            (3_000, false, failed),
            (4_000, true, None),
        ]
    );
    assert!(!store::ping(&c, 99, 5_000, false).unwrap());
}

fn reasons(c: &rusqlite::Connection, id: i64) -> Vec<Option<String>> {
    c.prepare("SELECT reason FROM checks WHERE monitor_id=?1 ORDER BY at")
        .unwrap()
        .query_map([id], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn a_sweep_marks_a_heartbeat_down_once_its_window_passes() {
    let c = memory();
    let id = backup(&c);
    let never = backup(&c);
    store::ping(&c, id, 1_000, false).unwrap();
    let deadline = 1_000 + 3_900_000;
    store::sweep(&c, deadline).unwrap();
    assert_eq!(status(&c, id).0, State::Up);
    store::sweep(&c, deadline + 1).unwrap();
    store::sweep(&c, deadline + 10_000).unwrap();
    assert_eq!(
        status(&c, id),
        (
            State::Down,
            Some(deadline + 1),
            Some(deadline + 1),
            Some(1_000)
        )
    );
    assert_eq!(reasons(&c, id), [None, Some("no ping in time".to_owned())]);
    assert_eq!(status(&c, never), (State::Pending, None, None, None));
}

#[test]
fn a_sweep_deletes_checks_older_than_30_days() {
    let c = memory();
    let id = backup(&c);
    let day = 86_400_000;
    let now = 40 * day;
    let expired = now - 30 * day - 1;
    c.execute(
        "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n LIMIT 1500)
         INSERT INTO checks(monitor_id,at,ok) SELECT ?1,?2,1 FROM n",
        rusqlite::params![id, expired],
    )
    .unwrap();
    for at in [now - 30 * day, now] {
        c.execute(
            "INSERT INTO checks(monitor_id,at,ok) VALUES (?1,?2,1)",
            rusqlite::params![id, at],
        )
        .unwrap();
    }
    let left = |c: &rusqlite::Connection| -> i64 {
        c.query_row("SELECT count(*) FROM checks", [], |r| r.get(0))
            .unwrap()
    };
    // 1,500 rows are two batches, and both fit in one sweep's budget.
    store::sweep(&c, now).unwrap();
    assert_eq!(left(&c), 2);
}

#[test]
fn an_incident_queues_monitor_down_and_then_monitor_up() {
    let c = memory();
    let id = backup(&c);
    let hook = CheckUrl::parse("https://hooks.example/x").unwrap();
    crate::alert::store::add(
        &c,
        Some(1),
        &crate::alert::Destination::Webhook(hook),
        false,
        0,
    )
    .unwrap();
    for (at, failed) in [(1_000, false), (2_000, true), (3_000, true), (4_000, false)] {
        store::ping(&c, id, at, failed).unwrap();
    }
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
            r#"{"id":1,"type":"monitor.down","project":"shop","at":"1970-01-01T00:00:02Z","monitor":{"id":1,"name":"Backup"},"incident":{"id":1,"reason":"failure ping"}}"#,
            r#"{"id":2,"type":"monitor.up","project":"shop","at":"1970-01-01T00:00:04Z","monitor":{"id":1,"name":"Backup"},"incident":{"id":1,"reason":"failure ping"}}"#,
        ]
    );
}

#[test]
fn an_http_monitor_is_due_each_interval_and_10_s_after_a_failure() {
    use super::check::{Checked, Http};
    let c = memory();
    let m = NewMonitor::new("API", http("http://a.test/", Expect::Status), None).unwrap();
    let id = store::add(&c, 1, &m, 0).unwrap().id;
    backup(&c);
    let due = |now| -> Vec<i64> { store::due(&c, now).unwrap().iter().map(|h| h.id).collect() };
    assert_eq!(
        store::due(&c, 0).unwrap(),
        [Http {
            id,
            url: CheckUrl::parse("http://a.test/").unwrap(),
            expect: Expect::Status,
            timeout: Duration::from_secs(10),
        }]
    );
    let up = Checked {
        outcome: Outcome::Up,
        status: Some(200),
        ms: 12,
    };
    assert!(store::checked(&c, id, 1000, &up).unwrap());
    assert_eq!(due(60_999), [] as [i64; 0]);
    assert_eq!(due(61_000), [id]);
    let down = Checked {
        outcome: Outcome::Down(Reason::Status(503)),
        status: Some(503),
        ms: 40,
    };
    assert!(store::checked(&c, id, 61_000, &down).unwrap());
    assert_eq!(status(&c, id).0, State::Failing);
    assert_eq!(due(70_999), [] as [i64; 0]);
    assert_eq!(due(71_000), [id]);
    let rows: Vec<(i64, bool, Option<i64>, Option<i64>)> = c
        .prepare("SELECT at,ok,status,ms FROM checks WHERE monitor_id=?1 ORDER BY at")
        .unwrap()
        .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        rows,
        [
            (1000, true, Some(200), Some(12)),
            (61_000, false, Some(503), Some(40))
        ]
    );
    assert!(!store::checked(&c, 99, 1000, &up).unwrap());
}
