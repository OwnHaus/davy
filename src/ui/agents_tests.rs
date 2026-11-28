//! The contract of the tokens and ledger routes.

use super::ui;
use crate::{access::Level, ledger, time, tokens};
use serde_json::{Value, json};

#[tokio::test]
async fn a_person_changes_a_token_and_the_ledger_names_them() {
    let s = ui();
    let now = time::now_ms();
    let name = tokens::Name::parse("ci").unwrap();
    tokens::create(&s.db, &name, Level::Triage, now)
        .unwrap()
        .unwrap();
    let cookie = s.person("ann@example.com", now);
    let post = async |body: Value| {
        let r = s
            .post("/ui/api/tokens", &[("cookie", &cookie)], &body)
            .await;
        (r.status, r.body)
    };
    assert_eq!(
        post(json!({"do": "unlock", "name": "ci"})).await,
        (
            409,
            json!({"error": "token \"ci\" has level triage: only a token of level full can be unlocked for destructive changes"})
        )
    );
    let (status, all) = post(json!({"do": "level", "name": "ci", "level": "full"})).await;
    assert_eq!((status, &all[0]["level"]), (200, &json!("full")));
    let (status, all) = post(json!({"do": "unlock", "name": "ci"})).await;
    assert_eq!(
        (status, &all[0]["destructive"], &all[0]["unlocked_by"]),
        (200, &json!(true), &json!("ann@example.com"))
    );
    let (_, all) = post(json!({"do": "lock", "name": "ci"})).await;
    assert_eq!(all[0]["destructive"], false);
    let (_, all) = post(json!({"do": "revoke", "name": "ci"})).await;
    assert!(all[0]["revoked_at"].is_string());
    assert_eq!(
        post(json!({"do": "lock", "name": "ci"})).await,
        (
            404,
            json!({"error": "no token named \"ci\", or it was revoked"})
        )
    );
    let (status, _) = post(json!({"do": "level", "name": "CI!", "level": "read"})).await;
    assert_eq!(status, 400);
    let (status, _) = post(json!({"do": "add", "name": "ci"})).await;
    assert_eq!(status, 400);

    let r = s.get("/ui/api/tokens", &[("cookie", &cookie)]).await;
    let listed: Vec<(&str, &str)> = r
        .body
        .as_array()
        .unwrap()
        .iter()
        .map(|t| (t["name"].as_str().unwrap(), t["level"].as_str().unwrap()))
        .collect();
    assert_eq!(listed, [("ci", "full")]);
    assert!(r.body[0].get("hash").is_none());

    let r = s.get("/ui/api/ledger", &[("cookie", &cookie)]).await;
    assert_eq!(r.status, 200);
    let ann = "ann@example.com";
    let refused = "token \"ci\" has level triage: only a token of level full can be unlocked for destructive changes";
    assert_eq!(
        token_rows(&r.body, ann),
        [
            json!([
                "token_lock",
                "error",
                "locked destructive changes; no token named \"ci\", or it was revoked"
            ]),
            json!(["token_revoke", "done", "revoked"]),
            json!(["token_lock", "done", "locked destructive changes"]),
            json!(["token_unlock", "done", "unlocked destructive changes"]),
            json!(["token_level", "done", "set level full"]),
            json!([
                "token_unlock",
                "error",
                format!("unlocked destructive changes; {refused}")
            ]),
        ]
    );
    assert_eq!(
        (&r.body["actors"], &r.body["next_before"]),
        (&json!([ann]), &Value::Null)
    );
}

#[tokio::test]
async fn the_ledger_pages_back_and_filters_by_actor() {
    let s = ui();
    let cookie = s.person("ann@example.com", time::now_ms());
    for (who, n) in [("bob@example.com", 30), ("ann@example.com", 30)] {
        for i in 0..n {
            let what = ledger::What {
                summary: format!("change {i}"),
                ..ledger::What::default()
            };
            let entry = ledger::Entry::of(ledger::Who::Person(who.into()), "issue_status", what);
            ledger::add(&s.db, &entry).unwrap();
        }
    }
    let page = async |q: &str| {
        s.get(&format!("/ui/api/ledger{q}"), &[("cookie", &cookie)])
            .await
            .body
    };
    let first = page("").await;
    assert_eq!(first["rows"].as_array().unwrap().len(), 50);
    assert_eq!(first["rows"][0]["summary"], "change 29");
    assert_eq!(
        first["actors"],
        json!(["ann@example.com", "bob@example.com"])
    );
    let before = first["next_before"].as_i64().unwrap();
    assert_eq!(before, 11);
    let second = page(&format!("?before={before}")).await;
    let summaries: Vec<&str> = second["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["summary"].as_str().unwrap())
        .collect();
    assert_eq!(
        summaries,
        [
            "change 9", "change 8", "change 7", "change 6", "change 5", "change 4", "change 3",
            "change 2", "change 1", "change 0"
        ]
    );
    assert_eq!(second["next_before"], Value::Null);
    let bob = page("?actor=bob%40example.com").await;
    assert_eq!(bob["rows"].as_array().unwrap().len(), 30);
    assert_eq!(bob["rows"][0]["actor"], "bob@example.com");
    assert_eq!(
        s.get("/ui/api/ledger?before=x", &[("cookie", &cookie)])
            .await
            .status,
        400
    );
}

/// The action, outcome and summary of each row of a ledger page, each
/// checked to be `person`'s change of token `ci`.
fn token_rows(page: &Value, person: &str) -> Vec<Value> {
    let rows = page["rows"].as_array().unwrap();
    rows.iter()
        .map(|r| {
            let who = (&r["actor_kind"], &r["actor"], &r["target"]);
            assert_eq!(who, (&json!("person"), &json!(person), &json!("token ci")));
            json!([r["action"], r["outcome"], r["summary"]])
        })
        .collect()
}
