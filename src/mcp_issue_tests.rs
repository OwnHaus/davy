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
