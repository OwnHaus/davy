//! The issue tools.

use super::*;

#[tokio::test]
async fn issue_context_shows_the_newest_event_of_an_issue_past_its_sample() {
    let s = server();
    let c = store::open(&s.data).unwrap();
    // The 51st event is the first outside the sample, so its payload is
    // kept only for being the newest.
    for n in 1..=51 {
        let raw = json!({"event_id": format!("{n:032x}"), "message": "boom"});
        let e = crate::event::ErrorEvent::parse(raw).unwrap();
        store::insert(&c, 1, &e).unwrap();
    }
    let (text, is_error) = tool(&s.app, "issue_context", json!({"issue_id": 1})).await;
    assert!(!is_error, "{text}");
    let context: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        (
            &context["latest_event"]["event_id"],
            &context["recent_events"][0]["event_id"]
        ),
        (
            &json!(format!("{:032x}", 51)),
            &json!(format!("{:032x}", 51))
        )
    );
}

#[tokio::test]
async fn no_tool_discards_an_issue() {
    let s = server();
    let c = store::open(&s.data).unwrap();
    let raw = json!({"event_id": format!("{:032x}", 1), "message": "boom"});
    store::insert(&c, 1, &crate::event::ErrorEvent::parse(raw).unwrap()).unwrap();
    let list = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"});
    let (_, body) = post(&s.app, "localhost", Some(SECRET), list).await;
    let reply: Value = serde_json::from_str(&body).unwrap();
    let names: Vec<&str> = reply["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), 17, "{names:?}");
    assert!(names.iter().all(|n| !n.contains("discard")), "{names:?}");
    let write = json!({"query": "UPDATE issues SET discarded_at=1 WHERE id=1"});
    let (text, is_error) = tool(&s.app, "sql", write).await;
    assert!(is_error, "{text}");
    let at: Option<i64> = c
        .query_row("SELECT discarded_at FROM issues WHERE id=1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(at, None);
}

#[tokio::test]
async fn issue_context_says_when_an_issue_was_discarded() {
    let s = server();
    let mut c = store::open(&s.data).unwrap();
    let raw = json!({"event_id": format!("{:032x}", 1), "message": "boom"});
    store::insert(&c, 1, &crate::event::ErrorEvent::parse(raw).unwrap()).unwrap();
    let context = async || {
        let (text, is_error) = tool(&s.app, "issue_context", json!({"issue_id": 1})).await;
        assert!(!is_error, "{text}");
        serde_json::from_str::<Value>(&text).unwrap()["issue"]["discarded_at"].clone()
    };
    assert_eq!(context().await, Value::Null);
    // 2026-11-25T10:00:00Z.
    crate::discard::set(&mut c, 1, false, 1_795_600_800_000).unwrap();
    assert_eq!(context().await, json!("2026-11-25T10:00:00Z"));
}
