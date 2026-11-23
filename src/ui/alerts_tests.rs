//! The contract of the channel and notification routes.

use super::{Ui, ui};
use crate::{store, time};
use rusqlite::params;
use serde_json::{Value, json};

/// A UI with a Slack and an e-mail channel, 52 notifications and a person,
/// whose cookie it returns.
fn alerting() -> (Ui, String) {
    let s = ui();
    let now = time::now_ms();
    store::create_project(&s.db, "shop").unwrap();
    s.db.execute_batch(
        "INSERT INTO channels(id,project_id,kind,url,secret,created_at)
         VALUES (1,1,'slack','https://hooks.slack.example/services/T0/B0/s3cret',NULL,0);
         INSERT INTO channels(id,project_id,kind,url,secret,created_at)
         VALUES (2,NULL,'email','mailto:ops@shop.example',NULL,0);",
    )
    .unwrap();
    let mut add = s
        .db
        .prepare(
            "INSERT INTO notifications(channel_id,type,payload,created_at,attempts,next_at,sent_at,failed_at,last_error)
             VALUES (?1,'issue.created','{}',?2,?3,?4,?5,?6,?7)",
        )
        .unwrap();
    add.execute(params![1, 1000, 1, 2000, 1500, None::<i64>, None::<String>])
        .unwrap();
    add.execute(params![1, 3000, 8, 9000, None::<i64>, 8000, "HTTP 500"])
        .unwrap();
    for i in 0..50 {
        add.execute(params![
            2,
            10_000 + i,
            0,
            10_000 + i,
            None::<i64>,
            None::<i64>,
            None::<String>
        ])
        .unwrap();
    }
    drop(add);
    let cookie = s.person("ann@example.com", now);
    (s, cookie)
}

#[tokio::test]
async fn channels_show_their_host_and_never_their_url() {
    let (s, cookie) = alerting();
    let r = s.get("/ui/api/channels", &[("cookie", &cookie)]).await;
    assert_eq!(
        r.body,
        json!([
            {"id": 1, "project": "shop", "kind": "slack", "to": "hooks.slack.example", "feedback": false, "unsent": 0, "failed": 1, "dropped": 0, "sdk_dropped": 0},
            {"id": 2, "project": null, "kind": "email", "to": "ops@shop.example", "feedback": false, "unsent": 50, "failed": 0, "dropped": 0, "sdk_dropped": 0},
        ])
    );
    assert!(!r.body.to_string().contains("s3cret"));
}

#[tokio::test]
async fn notifications_page_newest_first() {
    let (s, cookie) = alerting();
    let header = [("cookie", cookie.as_str())];
    let get = |uri: &'static str| s.get(uri, &header);

    let r = get("/ui/api/notifications").await;
    let page = r.body["notifications"].as_array().unwrap();
    assert_eq!(page.len(), 50);
    assert_eq!(page[0]["id"], 52);
    assert_eq!(
        page[0],
        json!({
            "id": 52, "channel": 2, "kind": "issue.created", "created": "1970-01-01T00:00:10.049Z",
            "delivery": "waiting", "attempts": 0, "done": null, "next_try": "1970-01-01T00:00:10.049Z", "last_error": null,
        })
    );
    assert_eq!(r.body["next"], "3");

    let r = get("/ui/api/notifications?cursor=3").await;
    assert_eq!(r.body["next"], Value::Null);
    assert_eq!(
        r.body["notifications"],
        json!([
            {"id": 2, "channel": 1, "kind": "issue.created", "created": "1970-01-01T00:00:03Z", "delivery": "failed",
             "attempts": 8, "done": "1970-01-01T00:00:08Z", "next_try": null, "last_error": "HTTP 500"},
            {"id": 1, "channel": 1, "kind": "issue.created", "created": "1970-01-01T00:00:01Z", "delivery": "sent",
             "attempts": 1, "done": "1970-01-01T00:00:01.5Z", "next_try": null, "last_error": null},
        ])
    );

    let r = get("/ui/api/notifications?channel=1").await;
    let ids: Vec<i64> = r.body["notifications"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_i64().unwrap())
        .collect();
    assert_eq!(ids, [2, 1]);

    for (uri, why) in [
        (
            "/ui/api/notifications?channel=x",
            "channel is the id of a channel",
        ),
        (
            "/ui/api/notifications?cursor=-1",
            "cursor is the next value of the page before",
        ),
        (
            "/ui/api/notifications?limit=5",
            "this route takes only channel, cursor",
        ),
    ] {
        let r = get(uri).await;
        assert_eq!((r.status, &r.body["error"]), (400, &json!(why)), "{uri}");
    }
}
