//! The retention tool.

use super::*;
use crate::retention::Setting;

#[tokio::test]
async fn set_retention_stores_the_change_and_shows_what_is_kept() {
    let s = server();
    let data = s.data.clone();
    let writer = std::thread::spawn(move || {
        let c = store::open(&data).unwrap();
        // A shorter setting is refused before the writer, see below.
        for _ in 0..2 {
            let w = next_job(&s.writes);
            let Job::SetRetention(change, by) = w.job else {
                panic!("expected a retention job");
            };
            assert_eq!(by, By::Agent(Defaults::default()));
            let stored = retention::set(&c, &change, by).unwrap();
            w.done.send(stored.map(drop)).unwrap();
        }
    });
    let (text, error) = tool(
        &s.app,
        "set_retention",
        json!({"project": "api", "logs_days": 90, "errors_days": "default"}),
    )
    .await;
    assert!(!error, "{text}");
    let kept: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(kept["project"], "api");
    assert_eq!(
        (&kept["logs"]["days"], &kept["logs"]["server_default"]),
        (&json!(90), &json!(false))
    );
    assert_eq!(
        (&kept["errors"]["days"], &kept["errors"]["server_default"]),
        (&json!(36_500), &json!(true))
    );
    assert!(
        kept["logs"]["deletes_before"]
            .as_str()
            .unwrap()
            .ends_with('Z')
    );
    let unknown = tool(
        &s.app,
        "set_retention",
        json!({"project": "web", "errors_days": 9}),
    )
    .await;
    assert_eq!(unknown, ("unknown project \"web\"".to_owned(), true));
    let (text, error) = tool(
        &s.app,
        "set_retention",
        json!({"project": "api", "logs_days": 7}),
    )
    .await;
    assert_eq!(
        (text.as_str(), error),
        (
            "an agent may only keep data longer: logs are kept 90 days and 7 would delete what is older for good. A person shortens it with `davy project retention` or in the web UI. set_retention needs level full with destructive changes unlocked, and the admin token holds full. The admin token never makes a destructive change: a person makes it with the CLI or the web UI, or unlocks it for an agent token of level full.",
            true
        )
    );
    writer.join().unwrap();
    // Refused before the writer too: nothing to change, or out of range.
    let (text, error) = tool(&s.app, "set_retention", json!({"project": "api"})).await;
    assert!(
        error && text.starts_with("give logs_days, errors_days or both"),
        "{text}"
    );
    let (text, error) = tool(
        &s.app,
        "set_retention",
        json!({"project": "api", "logs_days": 36_501}),
    )
    .await;
    assert_eq!(
        (text.as_str(), error),
        ("retention is 1 to 36500 days, not 36501", true)
    );
    let (_, error) = tool(
        &s.app,
        "set_retention",
        json!({"project": "api", "logs_days": "forever"}),
    )
    .await;
    assert!(error);
}

#[tokio::test]
async fn set_retention_refuses_an_agent_the_default_and_stores_none_of_the_change() {
    let s = server();
    let c = store::open(&s.data).unwrap();
    let own = Change {
        project: "api".into(),
        logs_days: Some(Setting::Days(90)),
        errors_days: Some(Setting::Days(400)),
    };
    retention::set(&c, &own, By::Person).unwrap().unwrap();
    // Refused before the writer, so no job reaches it.
    let refusal = "an agent may not return errors to the server default: the project keeps them 400 days of its own";
    // Alone, and with a longer logs setting that must not slip through.
    for change in [
        json!({"project": "api", "errors_days": "default"}),
        json!({"project": "api", "logs_days": 120, "errors_days": "default"}),
    ] {
        // No writer answers here, so a change that slipped past the
        // refusal would wait for ever without the timeout.
        let call = tool(&s.app, "set_retention", change);
        let (text, error) = tokio::time::timeout(std::time::Duration::from_secs(10), call)
            .await
            .expect("set_retention reached the writer instead of being refused");
        assert!(error && text.starts_with(refusal), "{text}");
    }
    s.settled().await;
    assert!(
        s.writes
            .try_iter()
            .all(|w| !matches!(w.job, Job::SetRetention(..)))
    );
    let kept = retention::stored(&c, "api").unwrap().unwrap();
    assert_eq!(
        (
            kept.logs_days.map(retention::Days::get),
            kept.errors_days.map(retention::Days::get)
        ),
        (Some(90), Some(400))
    );
}
