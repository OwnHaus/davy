//! The contract of the issue routes.

use super::{Reply, Ui, ui};
use crate::{event::ErrorEvent, store, test_support::fixture, time};
use serde_json::{Value, json};
use std::sync::Arc;

const NODE_EVENT: &str = "5ead9abbc8f44f31b35d8a435f101579";

/// A UI with project `shop` and a person, whose cookie it returns.
fn shop() -> (Ui, String) {
    let s = ui();
    store::create_project(&s.db, "shop").unwrap();
    let cookie = s.person("ann@example.com", time::now_ms());
    (s, cookie)
}

fn add(s: &Ui, raw: Value) {
    store::insert(&s.db, 1, &ErrorEvent::parse(raw).unwrap()).unwrap();
}

/// The event item of an envelope the Sentry Node SDK 10 sent.
fn node_event() -> Value {
    let body = String::from_utf8(fixture("sentry-node-10/4.body")).unwrap();
    serde_json::from_str(body.lines().nth(2).unwrap()).unwrap()
}

async fn read(s: &Ui, cookie: &str, uri: &str) -> Reply {
    s.get(uri, &[("cookie", cookie)]).await
}

fn titles(body: &Value) -> Vec<&str> {
    let issues = body["issues"].as_array().unwrap();
    issues
        .iter()
        .map(|i| i["title"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn the_issue_list_sorts_filters_and_refuses_unknown_parameters() {
    let (s, cookie) = shop();
    add(
        &s,
        json!({ "message": "alpha failed", "environment": "prod" }),
    );
    add(&s, json!({ "message": "beta failed" }));
    add(&s, json!({ "message": "beta failed" }));
    let r = read(&s, &cookie, "/ui/api/issues?sort=events&project=shop").await;
    assert_eq!(r.status, 200);
    assert_eq!(titles(&r.body), ["beta failed", "alpha failed"]);
    assert_eq!(r.body["next"], Value::Null);
    assert_eq!(r.body["issues"][0]["count"], 2);
    assert_eq!(r.body["issues"][0]["hourly"].as_array().unwrap().len(), 24);
    let r = read(
        &s,
        &cookie,
        "/ui/api/issues?environment=prod&q=ALPHA&status=unresolved&release=",
    )
    .await;
    assert_eq!(titles(&r.body), ["alpha failed"]);
    let refused = [
        (
            "/ui/api/issues?page=2",
            400,
            "this route takes only sort, project, status, environment, release, q, cursor",
        ),
        ("/ui/api/issues?q=a&q=b", 400, "q is given twice"),
        (
            "/ui/api/issues?sort=title",
            400,
            "sort is last_seen, events or first_seen",
        ),
        (
            "/ui/api/issues?status=open",
            400,
            "status is unresolved, resolved or ignored",
        ),
        (
            "/ui/api/issues?sort=events&cursor=l1.2",
            400,
            "the cursor is not from a list in this order",
        ),
        ("/ui/api/issues?project=blog", 404, "no project blog"),
        ("/ui/api/projects?x=1", 400, "this route takes only "),
    ];
    for (uri, status, error) in refused {
        let r = read(&s, &cookie, uri).await;
        assert_eq!(
            (r.status, &r.body),
            (status, &json!({ "error": error })),
            "{uri}"
        );
    }
    let long = format!("/ui/api/issues?q={}", "a".repeat(201));
    let r = s.get(&long, &[("cookie", &cookie)]).await;
    assert_eq!(
        r.body,
        json!({ "error": "the search text has at most 200 characters" })
    );
}

#[tokio::test]
async fn projects_list_their_newest_releases() {
    let (s, cookie) = shop();
    store::create_project(&s.db, "blog").unwrap();
    add(&s, json!({ "message": "boom", "release": "v1" }));
    let r = s.get("/ui/api/projects", &[("cookie", &cookie)]).await;
    assert_eq!(
        (r.status, r.body),
        (
            200,
            json!([{ "slug": "blog", "releases": [] }, { "slug": "shop", "releases": ["v1"] }])
        )
    );
}

#[tokio::test]
async fn an_issue_shows_its_events_its_latest_event_and_the_logs_around_it() {
    let (s, cookie) = shop();
    add(&s, node_event());
    let r = read(&s, &cookie, "/ui/api/issues/1").await;
    assert_eq!(r.status, 200);
    assert_eq!(r.body["issue"]["project"], "shop");
    assert_eq!(r.body["issue"]["status"], "unresolved");
    assert_eq!(r.body["issue"].get("last_seen_ms"), None);
    assert_eq!(
        r.body["releases"],
        json!([{ "release": "e2e@1.0.0", "events": 1, "last_seen": "2026-11-20T08:42:01.048Z" }])
    );
    let r = read(&s, &cookie, "/ui/api/issues/1/events").await;
    assert_eq!(
        r.body,
        json!({ "events": [{ "event_id": NODE_EVENT, "timestamp": "2026-11-20T08:42:01.048Z", "release": "e2e@1.0.0", "environment": "test", "stored": true }], "next": null })
    );
    let r = read(&s, &cookie, "/ui/api/issues/1/event").await;
    assert_eq!(r.status, 200);
    assert_eq!(r.body["event_id"], NODE_EVENT);
    let thrown = &r.body["exceptions"]["items"][0];
    assert_eq!(thrown["type"], "SyntaxError");
    assert_eq!(
        thrown["frames"]["items"][1]["context_line"],
        "    JSON.parse('{nope')"
    );
    let by_id = s
        .get(
            &format!("/ui/api/issues/1/event?event={NODE_EVENT}"),
            &[("cookie", &cookie)],
        )
        .await;
    assert_eq!(by_id.body, r.body);
    let r = read(&s, &cookie, "/ui/api/issues/1/logs").await;
    assert_eq!(
        (r.status, r.body),
        (
            200,
            json!({ "scope": "trace f9e1079522134d8194d62046d471f90c, one hour either side of the event", "logs": [], "cut": null })
        )
    );
    let refused = [
        ("/ui/api/issues/2", 404, "no issue 2"),
        ("/ui/api/issues/abc/events", 404, "no issue abc"),
        ("/ui/api/issues/2/event", 404, "no issue 2"),
        (
            "/ui/api/issues/1/event?event=nope",
            400,
            "event is an event id, 32 hex digits",
        ),
        (
            "/ui/api/issues/1/event?event=00000000000000000000000000000001",
            404,
            "Davy kept no payload of that event",
        ),
        (
            "/ui/api/issues/1/logs?event=00000000000000000000000000000001",
            404,
            "no such event of this issue",
        ),
        (
            "/ui/api/issues/1/events?cursor=-4",
            400,
            "the cursor is not from this list",
        ),
    ];
    for (uri, status, error) in refused {
        let r = read(&s, &cookie, uri).await;
        assert_eq!(
            (r.status, &r.body),
            (status, &json!({ "error": error })),
            "{uri}"
        );
    }
}

#[tokio::test]
async fn reading_an_event_waits_while_ingest_holds_the_memory() {
    let (s, cookie) = shop();
    add(&s, node_event());
    let mut held = s.ingest.hold();
    held.take(32 << 20).unwrap();
    let r = s
        .get("/ui/api/issues/1/event", &[("cookie", &cookie)])
        .await;
    assert_eq!((r.status, r.header("retry-after")), (429, "1"));
    assert_eq!(r.body, json!({ "error": "too many bodies in flight" }));
    drop(held);
    let r = s
        .get("/ui/api/issues/1/event", &[("cookie", &cookie)])
        .await;
    assert_eq!(r.status, 200);
}

#[tokio::test]
async fn a_ninth_log_search_at_once_waits() {
    let (s, cookie) = shop();
    add(&s, node_event());
    let busy = Arc::clone(&s.reads.searches)
        .try_acquire_many_owned(8)
        .unwrap();
    let r = s.get("/ui/api/issues/1/logs", &[("cookie", &cookie)]).await;
    assert_eq!((r.status, r.header("retry-after")), (429, "1"));
    assert_eq!(r.body, json!({ "error": "too many log searches at once" }));
    drop(busy);
    let r = s.get("/ui/api/issues/1/logs", &[("cookie", &cookie)]).await;
    assert_eq!(r.status, 200);
}

#[tokio::test]
async fn a_status_change_applies_to_every_named_issue_or_to_none() {
    let (s, cookie) = shop();
    add(&s, json!({ "message": "alpha failed" }));
    add(&s, json!({ "message": "beta failed" }));
    let post = async |body: Value| {
        let r = s
            .post("/ui/api/issues/status", &[("cookie", &cookie)], &body)
            .await;
        (r.status, r.body)
    };
    let status = |id: i64| {
        let row = store::issue(&s.db, id).unwrap().unwrap();
        (row.status.as_str(), row.resolved_in_release)
    };
    assert_eq!(
        post(json!({ "issues": [1, 2], "status": "ignored" })).await,
        (204, Value::Null)
    );
    assert_eq!(
        (status(1), status(2)),
        (("ignored", None), ("ignored", None))
    );
    let missing = post(json!({ "issues": [1, 99], "status": "resolved" })).await;
    assert_eq!(missing, (404, json!({ "error": "no issue 99" })));
    assert_eq!(status(1), ("ignored", None));
    let fixed = json!({ "issues": [2], "status": "resolved", "release": "v2" });
    assert_eq!(post(fixed).await, (204, Value::Null));
    assert_eq!(status(2), ("resolved", Some("v2".to_owned())));
    let refused = [
        (
            json!({ "issues": [1], "status": "ignored", "release": "v2" }),
            "a release goes only with resolved",
        ),
        (
            json!({ "issues": [], "status": "ignored" }),
            "name at least one issue",
        ),
        (
            json!({ "issues": (1..=501).collect::<Vec<_>>(), "status": "ignored" }),
            "a change names at most 500 issues, this one 501",
        ),
        (
            json!({ "issues": [1], "status": "resolved", "release": "v".repeat(201) }),
            "a release has at most 200 bytes",
        ),
    ];
    for (body, error) in refused {
        assert_eq!(post(body).await, (400, json!({ "error": error })));
    }
    let (code, unknown) = post(json!({ "issues": [1], "status": "ignored", "why": "x" })).await;
    assert_eq!(code, 400);
    assert!(
        unknown["error"]
            .as_str()
            .unwrap()
            .starts_with("unknown field `why`")
    );
}

#[tokio::test]
async fn the_event_of_an_issue_past_its_sample_is_its_newest() {
    let (s, cookie) = shop();
    // 52 events: the first 50 keep their payload, 51 lost its own when 52
    // came, and 52, outside the sample, is kept for being the newest.
    for n in 1..=52 {
        add(
            &s,
            json!({ "event_id": format!("{n:032x}"), "message": "boom" }),
        );
    }
    let r = read(&s, &cookie, "/ui/api/issues/1/event").await;
    assert_eq!(
        (r.status, &r.body["event_id"]),
        (200, &json!(format!("{:032x}", 52)))
    );
    let named = format!("/ui/api/issues/1/event?event={:032x}", 52);
    assert_eq!(read(&s, &cookie, &named).await.body, r.body);
    let gone = read(
        &s,
        &cookie,
        &format!("/ui/api/issues/1/event?event={:032x}", 51),
    )
    .await;
    assert_eq!(
        (gone.status, gone.body),
        (
            404,
            json!({ "error": "Davy kept no payload of that event" })
        )
    );
}
