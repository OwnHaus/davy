//! The contract of the connect, setup and retention routes.

use super::{Ui, ui};
use crate::{
    event::ErrorEvent,
    firsts::{self, First},
    http::body::Budget,
    store::{self, Job},
    time,
};
use serde_json::{Value, json};

#[tokio::test]
async fn connect_names_every_endpoint_and_each_project_dsn() {
    let s = ui();
    let (_, shop) = store::create_project(&s.db, "shop").unwrap();
    let (_, admin) = store::create_project(&s.db, "admin").unwrap();
    let cookie = s.person("ann@example.com", time::now_ms());
    let r = s.get("/ui/api/connect", &[("cookie", &cookie)]).await;
    assert_eq!(
        r.body,
        json!({
            "public_url": "https://davy.example",
            "logs": "https://davy.example/",
            "otlp": "https://davy.example/v1/logs",
            "mcp": "https://davy.example/mcp",
            "projects": [
                {"slug": "admin", "dsn": format!("https://{admin}@davy.example/2"), "key": admin},
                {"slug": "shop", "dsn": format!("https://{shop}@davy.example/1"), "key": shop},
            ],
        })
    );
}

async fn steps(s: &Ui, cookie: &str) -> Vec<(String, Value)> {
    let r = s.get("/ui/api/setup", &[("cookie", cookie)]).await;
    assert_eq!(r.status, 200);
    r.body["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|step| {
            (
                step["what"].as_str().unwrap().to_owned(),
                step["at"].clone(),
            )
        })
        .collect()
}

fn done(steps: &[(String, Value)]) -> Vec<&str> {
    steps
        .iter()
        .filter(|(_, at)| !at.is_null())
        .map(|(what, _)| what.as_str())
        .collect()
}

#[tokio::test]
async fn setup_marks_each_step_once_and_keeps_it_done() {
    let s = ui();
    let cookie = s.person("ann@example.com", time::now_ms());
    let r = s.get("/ui/api/setup", &[("cookie", &cookie)]).await;
    assert_eq!(
        r.body,
        json!({"steps": [
            {"what": "agent", "required": true, "at": null},
            {"what": "project", "required": true, "at": null},
            {"what": "error", "required": true, "at": null},
            {"what": "symbolicated", "required": true, "at": null},
            {"what": "log", "required": false, "at": null},
            {"what": "monitor", "required": false, "at": null},
            {"what": "delivered", "required": false, "at": null},
        ]})
    );

    firsts::record(&s.db, First::Agent, 1_000).unwrap();
    firsts::record(&s.db, First::Agent, 2_000).unwrap();
    store::create_project(&s.db, "shop").unwrap();
    let event = ErrorEvent {
        symbolicated: true,
        ..ErrorEvent::parse(json!({"message": "boom"})).unwrap()
    };
    let job = Job::Ingest {
        project: 1,
        events: vec![event],
        held: Budget::new(0).hold(),
    };
    store::submit(&s.writer.sender, job).await.unwrap();
    s.db.execute_batch(
        "INSERT INTO monitors(project_id,name,kind,url,every_s,state,created_at)
         VALUES (1,'home','http','https://shop.example/',60,'pending',5000)",
    )
    .unwrap();
    let found = steps(&s, &cookie).await;
    assert_eq!(
        done(&found),
        ["agent", "project", "error", "symbolicated", "monitor"]
    );
    assert_eq!(found[0].1, "1970-01-01T00:00:01Z", "the first agent counts");
    assert_eq!(found[5].1, "1970-01-01T00:00:05Z");

    s.db.execute_batch("DELETE FROM monitors").unwrap();
    let kept = steps(&s, &cookie).await;
    assert_eq!(kept, found, "a step stays done when its rows are gone");
}

#[tokio::test]
async fn retention_shows_every_project_and_changes_one() {
    let s = ui();
    store::create_project(&s.db, "shop").unwrap();
    store::create_project(&s.db, "admin").unwrap();
    let cookie = s.person("ann@example.com", time::now_ms());
    let r = s.get("/ui/api/retention", &[("cookie", &cookie)]).await;
    assert_eq!(r.status, 200);
    let slugs: Vec<&Value> = r
        .body
        .as_array()
        .unwrap()
        .iter()
        .map(|p| &p["project"])
        .collect();
    assert_eq!(slugs, [&json!("admin"), &json!("shop")]);
    assert_eq!(r.body[1]["errors"]["days"], 36_500);
    assert_eq!(r.body[1]["logs"]["server_default"], true);
    let post = async |body: Value| {
        let r = s
            .post("/ui/api/retention", &[("cookie", &cookie)], &body)
            .await;
        (r.status, r.body)
    };
    let (status, kept) = post(json!({"project": "shop", "errors_days": 90, "logs_days": 7})).await;
    assert_eq!(status, 200, "{kept}");
    assert_eq!(kept["project"], "shop");
    assert_eq!(
        (&kept["errors"]["days"], &kept["errors"]["server_default"]),
        (&json!(90), &json!(false))
    );
    assert_eq!(kept["logs"]["days"], 7);
    let (_, kept) = post(json!({"project": "shop", "logs_days": "default"})).await;
    assert_eq!(
        (&kept["logs"]["days"], &kept["errors"]["days"]),
        (&json!(30), &json!(90))
    );
    assert_eq!(
        post(json!({"project": "gone", "errors_days": 9})).await,
        (404, json!({"error": "unknown project \"gone\""}))
    );
    assert_eq!(
        post(json!({"project": "shop", "errors_days": 0})).await,
        (400, json!({"error": "retention is 1 to 36500 days, not 0"}))
    );
    let (status, _) = post(json!({"project": "shop", "errors_days": "forever"})).await;
    assert_eq!(status, 400);
    let r = s.get("/ui/api/retention?x=1", &[("cookie", &cookie)]).await;
    assert_eq!(r.status, 400);
}
