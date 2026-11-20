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
