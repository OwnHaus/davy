use super::*;
use crate::monitor::CheckUrl;

fn memory() -> rusqlite::Connection {
    let mut c = rusqlite::Connection::open_in_memory().unwrap();
    crate::store::init(&mut c).unwrap();
    crate::store::create_project(&c, "shop").unwrap();
    c
}

fn url(s: &str) -> CheckUrl {
    CheckUrl::parse(s).unwrap()
}

#[test]
fn channels_are_added_listed_and_removed() {
    let c = memory();
    let hook = store::add(
        &c,
        Some(1),
        Kind::Webhook,
        &url("https://hooks.example/davy"),
        1_000,
    )
    .unwrap();
    assert_eq!(hook.id, 1);
    let secret = hook.secret.unwrap();
    assert_eq!(secret.as_str().len(), 64);
    let slack = store::add(
        &c,
        None,
        Kind::Slack,
        &url("https://hooks.slack.com/services/T0/B0/x"),
        2_000,
    )
    .unwrap();
    assert_eq!(
        slack,
        store::Added {
            id: 2,
            secret: None
        }
    );
    let listed = store::list(&c).unwrap();
    assert_eq!(
        listed,
        [
            store::Listed {
                id: 1,
                project: Some("shop".into()),
                kind: Kind::Webhook,
                host: "hooks.example".into(),
                unsent: 0,
                failed: 0,
                dropped: 0
            },
            store::Listed {
                id: 2,
                project: None,
                kind: Kind::Slack,
                host: "hooks.slack.com".into(),
                unsent: 0,
                failed: 0,
                dropped: 0
            },
        ]
    );
    assert!(store::remove(&c, 2).unwrap());
    assert!(!store::remove(&c, 2).unwrap());
    assert_eq!(store::list(&c).unwrap().len(), 1);
}

#[test]
fn a_webhook_secret_is_64_hex_characters_and_new_each_time() {
    let c = memory();
    let secret = || {
        store::add(&c, None, Kind::Webhook, &url("https://hooks.example/"), 0)
            .unwrap()
            .secret
            .unwrap()
    };
    let (first, second) = (secret(), secret());
    assert!(
        first
            .as_str()
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
        "{first:?}"
    );
    assert_eq!(first.as_str().len(), 64);
    assert_ne!(first, second);
}

#[test]
fn a_data_directory_holds_at_most_100_channels() {
    let c = memory();
    c.execute_batch(
        "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n LIMIT 100)
         INSERT INTO channels(kind,url,created_at) SELECT 'slack','https://hooks.slack.com/',0 FROM n",
    )
    .unwrap();
    let refused = store::add(&c, None, Kind::Slack, &url("https://hooks.slack.com/"), 0);
    assert_eq!(
        refused.unwrap_err().to_string(),
        "a data directory holds at most 100 channels"
    );
}

fn issue(title: &str) -> Issue {
    Issue {
        id: 3,
        title: title.into(),
        culprit: Some("parseOrder(src/app.js)".into()),
        level: "error".into(),
        release: Some("shop@2.0.0".into()),
    }
}

fn incident() -> Incident {
    Incident {
        id: 4,
        monitor: 1,
        name: "API".into(),
        reason: "HTTP 503".into(),
    }
}

#[test]
fn a_webhook_body_carries_its_delivery_id_and_the_notice() {
    let new = Notice::IssueNew(issue("TypeError: x is undefined"));
    assert_eq!(
        new.render(Kind::Webhook, 18, Some("shop"), 1_795_164_121_000)
            .unwrap(),
        r#"{"id":18,"type":"issue.new","project":"shop","at":"2026-11-20T08:42:01Z","issue":{"id":3,"title":"TypeError: x is undefined","culprit":"parseOrder(src/app.js)","level":"error","release":"shop@2.0.0"}}"#
    );
    let down = Notice::MonitorDown(incident());
    assert_eq!(
        down.render(Kind::Webhook, 19, Some("shop"), 1_795_164_190_000)
            .unwrap(),
        r#"{"id":19,"type":"monitor.down","project":"shop","at":"2026-11-20T08:43:10Z","monitor":{"id":1,"name":"API"},"incident":{"id":4,"reason":"HTTP 503"}}"#
    );
    assert_eq!(
        Notice::Test.render(Kind::Webhook, 20, None, 1_000).unwrap(),
        r#"{"id":20,"type":"test","project":null,"at":"1970-01-01T00:00:01Z"}"#
    );
}

#[test]
fn a_slack_message_is_one_escaped_line() {
    let slack = |project, notice: Notice| notice.render(Kind::Slack, 1, project, 0).unwrap();
    let cases = [
        (
            slack(Some("shop"), Notice::IssueNew(issue("TypeError: x"))),
            r#"{"text":"[shop] New issue: TypeError: x"}"#,
        ),
        (
            slack(Some("shop"), Notice::IssueRegressed(issue("TypeError: x"))),
            r#"{"text":"[shop] Issue regressed: TypeError: x"}"#,
        ),
        (
            slack(Some("shop"), Notice::MonitorDown(incident())),
            r#"{"text":"[shop] API is down: HTTP 503"}"#,
        ),
        (
            slack(Some("shop"), Notice::MonitorUp(incident())),
            r#"{"text":"[shop] API is up again"}"#,
        ),
        (
            slack(None, Notice::Test),
            r#"{"text":"Test notice from Davy"}"#,
        ),
        (
            slack(Some("shop"), Notice::IssueNew(issue("<!channel> & <@U1>"))),
            r#"{"text":"[shop] New issue: &lt;!channel&gt; &amp; &lt;@U1&gt;"}"#,
        ),
    ];
    for (got, want) in cases {
        assert_eq!(got, want);
    }
    let long = slack(Some("shop"), Notice::IssueNew(issue(&"é".repeat(300))));
    assert_eq!(
        long,
        format!(r#"{{"text":"[shop] New issue: {}"}}"#, "é".repeat(200))
    );
}

fn channel(c: &rusqlite::Connection, project: Option<i64>, kind: Kind) -> i64 {
    store::add(c, project, kind, &url("https://hooks.example/x"), 0)
        .unwrap()
        .id
}

fn queued(c: &rusqlite::Connection) -> Vec<(i64, i64, String, String, i64)> {
    c.prepare("SELECT id,channel_id,type,payload,next_at FROM notifications ORDER BY id")
        .unwrap()
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

#[test]
fn a_notice_queues_one_row_for_each_channel_of_its_project() {
    let c = memory();
    crate::store::create_project(&c, "blog").unwrap();
    let shop = channel(&c, Some(1), Kind::Webhook);
    let all = channel(&c, None, Kind::Slack);
    channel(&c, Some(2), Kind::Webhook);
    store::notify(&c, 1, &Notice::MonitorDown(incident()), 5_000).unwrap();
    store::notify(&c, 1, &Notice::MonitorUp(incident()), 6_000).unwrap();
    let rows = queued(&c);
    let summary: Vec<_> = rows
        .iter()
        .map(|(id, ch, kind, _, next_at)| (*id, *ch, kind.as_str(), *next_at))
        .collect();
    assert_eq!(
        summary,
        [
            (1, shop, "monitor.down", 5_000),
            (2, all, "monitor.down", 5_000),
            (3, shop, "monitor.up", 6_000),
            (4, all, "monitor.up", 6_000),
        ]
    );
    assert_eq!(
        rows[2].3,
        r#"{"id":3,"type":"monitor.up","project":"shop","at":"1970-01-01T00:00:06Z","monitor":{"id":1,"name":"API"},"incident":{"id":4,"reason":"HTTP 503"}}"#
    );
    assert_eq!(rows[3].3, r#"{"text":"[shop] API is up again"}"#);
}

#[test]
fn a_channel_with_100_unsent_notices_drops_the_next_and_counts_it() {
    let c = memory();
    let id = channel(&c, Some(1), Kind::Slack);
    for at in 0..101 {
        store::notify(&c, 1, &Notice::Test, at).unwrap();
    }
    let listed = store::list(&c).unwrap();
    assert_eq!((listed[0].unsent, listed[0].dropped), (100, 1));
    c.execute("UPDATE notifications SET sent_at=1 WHERE id=1", [])
        .unwrap();
    store::notify(&c, 1, &Notice::Test, 200).unwrap();
    let listed = store::list(&c).unwrap();
    assert_eq!(
        (listed[0].id, listed[0].unsent, listed[0].dropped),
        (id, 100, 1)
    );
}

#[test]
fn a_test_notice_goes_to_the_one_channel_it_names() {
    let c = memory();
    channel(&c, Some(1), Kind::Slack);
    let all = channel(&c, None, Kind::Webhook);
    assert!(store::test(&c, all, 7_000).unwrap());
    assert!(!store::test(&c, 9, 7_000).unwrap());
    let rows = queued(&c);
    assert_eq!(
        rows,
        [(
            1,
            all,
            "test".into(),
            r#"{"id":1,"type":"test","project":null,"at":"1970-01-01T00:00:07Z"}"#.into(),
            7_000
        )]
    );
}

#[test]
fn a_signature_is_the_hmac_of_the_time_and_body_under_the_secret() {
    let secret = Secret::load("a1".repeat(32));
    assert_eq!(
        secret
            .sign(1_795_164_121, r#"{"id":18,"type":"test"}"#)
            .unwrap(),
        "t=1795164121,v1=7f853164c56305474a83e2915c371a155f004fcf1e8566e8b3fa968f58271262"
    );
}

#[test]
fn a_failed_delivery_retries_five_times_with_doubling_waits() {
    let waits: Vec<Option<u64>> = (1..=7)
        .map(|failures| retry_after(failures).map(|d| d.as_secs()))
        .collect();
    assert_eq!(
        waits,
        [
            Some(30),
            Some(60),
            Some(120),
            Some(240),
            Some(480),
            None,
            None
        ]
    );
}

fn row(c: &rusqlite::Connection, id: i64) -> (i64, i64, Option<i64>, Option<i64>, Option<String>) {
    c.query_row(
        "SELECT attempts,next_at,sent_at,failed_at,last_error FROM notifications WHERE id=?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    )
    .unwrap()
}

#[test]
fn only_the_oldest_unsent_row_of_each_channel_is_due() {
    let c = memory();
    let hook = channel(&c, Some(1), Kind::Webhook);
    let slack = channel(&c, None, Kind::Slack);
    store::notify(&c, 1, &Notice::Test, 1_000).unwrap();
    store::notify(&c, 1, &Notice::Test, 2_000).unwrap();
    c.execute("UPDATE notifications SET next_at=5000 WHERE id=2", [])
        .unwrap();
    let due = |at| -> Vec<(i64, i64, &'static str)> {
        store::due(&c, at)
            .unwrap()
            .into_iter()
            .map(|d| {
                let kind = match d.to {
                    Channel::Webhook { secret, .. } if secret.as_str().len() == 64 => "webhook",
                    Channel::Webhook { .. } => "webhook without its secret",
                    Channel::Slack { .. } => "slack",
                };
                (d.id, d.channel, kind)
            })
            .collect()
    };
    assert_eq!(due(3_000), [(1, hook, "webhook")]);
    assert_eq!(due(5_000), [(1, hook, "webhook"), (2, slack, "slack")]);
    store::delivered(&c, 1, 5_000, &Ok(())).unwrap();
    assert_eq!(due(5_000), [(2, slack, "slack"), (3, hook, "webhook")]);
}

#[test]
fn a_failed_row_waits_longer_each_time_then_fails_and_a_sent_row_stays_sent() {
    let c = memory();
    channel(&c, Some(1), Kind::Slack);
    store::notify(&c, 1, &Notice::Test, 0).unwrap();
    let failure = Err("HTTP 500".to_owned());
    let mut seen = Vec::new();
    for at in [1_000, 40_000, 110_000, 240_000, 500_000, 990_000] {
        store::delivered(&c, 1, at, &failure).unwrap();
        let (attempts, next_at, sent, failed, error) = row(&c, 1);
        assert_eq!((sent, error.as_deref()), (None, Some("HTTP 500")));
        seen.push((attempts, next_at, failed));
    }
    assert_eq!(
        seen,
        [
            (1, 31_000, None),
            (2, 100_000, None),
            (3, 230_000, None),
            (4, 480_000, None),
            (5, 980_000, None),
            (6, 980_000, Some(990_000)),
        ]
    );
    store::notify(&c, 1, &Notice::Test, 0).unwrap();
    store::delivered(&c, 2, 1_000, &Err("x".repeat(300))).unwrap();
    assert_eq!(row(&c, 2).4.unwrap().len(), 200);
    store::delivered(&c, 2, 2_000, &Ok(())).unwrap();
    assert_eq!(row(&c, 2).0, 2);
    assert_eq!(row(&c, 2).2, Some(2_000));
    store::delivered(&c, 99, 2_000, &Err("gone".into())).unwrap();
}

#[test]
fn a_secret_never_shows_in_debug_output() {
    assert_eq!(format!("{:?}", Secret::load("a1".repeat(32))), "Secret(..)");
}
