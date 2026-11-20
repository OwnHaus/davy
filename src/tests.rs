use crate::{envelope::*, event::*, grouping::*, store};
use serde_json::json;
#[test]
fn fixtures() {
    for sdk in ["sentry-node-7", "sentry-node-10"] {
        for n in 1..=4 {
            let p = format!("{}/e2e/fixtures/{sdk}/{n}.body", env!("CARGO_MANIFEST_DIR"));
            let e = Envelope::parse(&std::fs::read(p).unwrap()).unwrap();
            let kinds: Vec<_> = e.items.iter().map(|i| &i.kind).collect();
            assert_eq!(
                kinds,
                if n == 1 || n == 3 {
                    vec![&ItemKind::Session]
                } else {
                    vec![&ItemKind::Event]
                }
            );
            if n == 4 {
                let event = ErrorEvent::parse(serde_json::from_slice(&e.items[0].payload).unwrap())
                    .unwrap();
                assert_eq!(event.exceptions.len(), 1);
                assert_eq!(event.exceptions[0].ty, "SyntaxError");
                assert_eq!(
                    event.exceptions[0].value.as_deref(),
                    Some("Expected property name or '}' in JSON at position 1 (line 1 column 2)")
                );
            }
        }
    }
}
#[test]
fn fixture_transactions_are_dropped() {
    for n in [5, 6] {
        let p = format!(
            "{}/e2e/fixtures/sentry-node-10/{n}.body",
            env!("CARGO_MANIFEST_DIR")
        );
        let e = Envelope::parse(&std::fs::read(p).unwrap()).unwrap();
        assert_eq!(e.items.len(), 1);
        assert_eq!(e.items[0].kind, ItemKind::Transaction);
        assert_eq!(
            disposition(&e.items[0].kind),
            Disposition::Drop(DataCategory::Transaction)
        );
    }
}
fn event(v: serde_json::Value) -> ErrorEvent {
    ErrorEvent::parse(v).unwrap()
}
#[test]
fn grouping_rules() {
    let a = group(&event(
        json!({"message":"job 123 550e8400-e29b-41d4-a716-446655440000"}),
    ));
    let b = group(&event(
        json!({"message":"job 999 123e4567-e89b-12d3-a456-426614174000"}),
    ));
    assert_eq!(a.version, 1);
    assert_eq!(a.fingerprint, b.fingerprint);
    assert_eq!(a.fingerprint.len(), 32);
    let ex = |ty, line| {
        event(
            json!({"exception":{"values":[{"type":ty,"value":"oops", "stacktrace":{"frames":[{"filename":"main.3f9a1c2b.js?x=1","function":"crash","lineno":line,"in_app":true}]}}]}}),
        )
    };
    let a = group(&ex("TypeError", 1));
    assert_eq!(a.fingerprint, group(&ex("TypeError", 99)).fingerprint);
    assert_ne!(a.fingerprint, group(&ex("SyntaxError", 1)).fingerprint);
    assert_eq!(a.title, "TypeError: oops");
    assert_eq!(a.culprit.as_deref(), Some("crash"));
    let mut e = ex("TypeError", 1);
    e.fingerprint = Some(vec!["{{ default }}".into(), "x".into()]);
    assert_ne!(a.fingerprint, group(&e).fingerprint);
    assert_eq!(
        normalize("id 3 abcdef0123 a@example.com 10.0.0.1"),
        "id <number> <hex> <email> <ip>"
    );
    let a = event(json!({"logentry":{"message":"job %s","formatted":"job a"}}));
    let b = event(json!({"logentry":{"message":"job %s","formatted":"job b"}}));
    assert_eq!(group(&a).fingerprint, group(&b).fingerprint);
}
#[test]
fn envelope_binary_and_policy() {
    let e = Envelope::parse(
        b"{}\n{\"type\":\"attachment\",\"length\":3}\na\nb\n{\"type\":\"future\"}\nx",
    )
    .unwrap();
    assert_eq!(e.items[0].payload.as_ref(), b"a\nb");
    assert_eq!(e.items[1].kind, ItemKind::Other("future".into()));
    assert_eq!(
        disposition(&e.items[1].kind),
        Disposition::Drop(DataCategory::Unknown)
    );
    assert!(Envelope::parse(b"{}\n{\"type\":\"event\",\"length\":100}\nx").is_err());
    let e = Envelope::parse(b"{}\n\n{\"type\":\"session\"}\n{}\r\n\n{\"type\":\"event\"}\n{}\n\n")
        .unwrap();
    let kinds: Vec<_> = e.items.iter().map(|i| &i.kind).collect();
    assert_eq!(kinds, vec![&ItemKind::Session, &ItemKind::Event]);
}
#[test]
fn event_boundary() {
    let e = event(
        json!({"event_id":"550E8400-E29B-41D4-A716-446655440000","timestamp":"2020-01-01T00:00:00Z","message":{"formatted":"hi","message":"template"},"exception":[{"type":"Error"}]}),
    );
    assert_eq!(e.event_id.0, "550e8400e29b41d4a716446655440000");
    assert_eq!(e.timestamp, 1577836800000);
    assert_eq!(e.message.as_deref(), Some("hi"));
    assert_eq!(e.message_template.as_deref(), Some("template"));
    assert!(ErrorEvent::parse(json!({"event_id":"bad"})).is_err());
    let e = event(
        json!({"level":"warn","timestamp":"2020-01-01T00:00:00","fingerprint":["a",1,null],"exception":{"values":[{"type":"E","value":7,"stacktrace":{"frames":[{"function":"f","lineno":-1}]}}]}}),
    );
    assert_eq!(e.level, Level::Warning);
    assert_eq!(e.timestamp, 1577836800000);
    assert_eq!(e.fingerprint, Some(vec!["a".to_owned(), "1".to_owned()]));
    assert_eq!(e.exceptions[0].value, None);
    assert_eq!(e.exceptions[0].frames[0].function.as_deref(), Some("f"));
    assert_eq!(e.exceptions[0].frames[0].lineno, None);
    let e = event(json!({"timestamp":"yesterday","level":7}));
    assert_eq!(e.level, Level::Error);
    assert!(e.timestamp > 1577836800000);
}
fn memory() -> rusqlite::Connection {
    let c = rusqlite::Connection::open_in_memory().unwrap();
    c.execute_batch(store::SCHEMA).unwrap();
    c
}
#[test]
fn storage_regression_duplicates_sampling_and_ignored() {
    let mut c = memory();
    store::create_project(&c, "storage").unwrap();
    let first = event(json!({"message":"failure 1"}));
    {
        let tx = c.transaction().unwrap();
        store::insert(&tx, 1, &first).unwrap();
        tx.commit().unwrap();
    }
    c.execute(
        "UPDATE issues SET status='resolved',resolved_at=1,resolved_in_release='v1'",
        [],
    )
    .unwrap();
    store::insert(&c, 1, &first).unwrap();
    assert_eq!(
        c.query_row("SELECT status FROM issues", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "resolved"
    );
    let next = event(json!({"message":"failure 2"}));
    store::insert(&c, 1, &next).unwrap();
    let state: (String, i64, Option<i64>) = c
        .query_row(
            "SELECT status,regressed_at,resolved_at FROM issues",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(state, ("unresolved".into(), next.received_at, None));
    c.execute("UPDATE issues SET status='ignored'", []).unwrap();
    for _ in 2..101 {
        store::insert(&c, 1, &event(json!({"message":"failure 3"}))).unwrap();
    }
    assert_eq!(
        c.query_row("SELECT status,event_count FROM issues", [], |r| Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?
        )))
        .unwrap(),
        ("ignored".into(), 101)
    );
    assert_eq!(
        c.query_row("SELECT count(payload) FROM events", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        51
    );
    assert_eq!(
        c.query_row("SELECT sum(count) FROM issue_hourly", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        101
    );
    assert_eq!(store::payload(&c, 1).unwrap()["message"], "failure 3");
}
#[tokio::test]
async fn http_contract() {
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use std::sync::{Arc, mpsc};
    use tower::ServiceExt;
    let c = memory();
    store::create_project(&c, "test").unwrap();
    c.execute("UPDATE projects SET public_key='key'", [])
        .unwrap();
    let (sender, rx) = mpsc::sync_channel(1);
    let (logs, _logs_rx) = crate::logs::store::LogQueue::bounded(1);
    let app = crate::http::router(crate::http::App {
        keys: Arc::new(crate::http::Keys::new(c)),
        sender: sender.clone(),
        logs,
    });
    for (key, status) in [("", 401), ("?sentry_key=wrong", 403)] {
        let r = app
            .clone()
            .oneshot(
                Request::post(format!("/api/1/envelope/{key}"))
                    .body(Body::from("{}\n{\"type\":\"session\"}\n{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status().as_u16(), status);
        assert_eq!(r.headers()["x-sentry-rate-limits"], crate::http::RATE);
    }
    let r = app
        .clone()
        .oneshot(
            Request::post("/api/1/envelope/?sentry_key=wrong")
                .header("content-encoding", "gzip")
                .body(Body::from("not gzip"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status(), 403, "key is checked before the body is decoded");
    let r = app
        .clone()
        .oneshot(
            Request::post("/api/1/envelope/?sentry_key=key")
                .body(Body::from("{}\n{\"type\":\"future\"}\n{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let body = to_bytes(r.into_body(), 1000).await.unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["id"]
            .as_str()
            .unwrap()
            .len(),
        32
    );
    let (done, _) = tokio::sync::oneshot::channel();
    sender
        .try_send(store::Ingest {
            project: 1,
            events: vec![],
            done,
        })
        .unwrap();
    let r = app
        .clone()
        .oneshot(
            Request::post("/api/1/store/?sentry_key=key")
                .body(Body::from("{\"message\":\"test\"}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status(), 429);
    assert_eq!(r.headers()["retry-after"], "1");
    drop(rx);
    let r = app
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri("/api/1/store/")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status(), 204);
    assert_eq!(r.headers()["access-control-allow-origin"], "*");
}

#[test]
fn grouping_frame_selection_and_bundle_names() {
    let make = |filename: &str, function: &str, in_app: bool| json!({"filename":filename,"function":function,"in_app":in_app});
    let mut frames = vec![
        make("vendor.js", "vendor", false),
        make("main.3f9a1c2b.js?build=1", "crash", true),
    ];
    let with_frames = |frames: &Vec<serde_json::Value>| {
        event(
            json!({"exception":{"values":[{"type":"Error","value":"failed","stacktrace":{"frames":frames}}]}}),
        )
    };
    let a = group(&with_frames(&frames));
    frames[0] = make("different.js", "different", false);
    frames[1] = make("main.abcdef01.js?build=99", "crash", true);
    assert_eq!(a.fingerprint, group(&with_frames(&frames)).fingerprint);
    let mut frames: Vec<_> = (0..11)
        .map(|i| make("app.js", &format!("frame{i}"), true))
        .collect();
    let a = group(&with_frames(&frames));
    frames[0] = make("other.js", "excluded", true);
    assert_eq!(a.fingerprint, group(&with_frames(&frames)).fingerprint);
    frames[10] = make("app.js", "changed", true);
    assert_ne!(a.fingerprint, group(&with_frames(&frames)).fingerprint);
}

#[tokio::test]
async fn http_log_routes() {
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use std::sync::{Arc, mpsc};
    use tower::ServiceExt;
    let c = memory();
    store::create_project(&c, "a").unwrap();
    store::create_project(&c, "b").unwrap();
    c.execute("UPDATE projects SET public_key='key-' || slug", [])
        .unwrap();
    let (sender, _rx) = mpsc::sync_channel(1);
    let (logs, logs_rx) = crate::logs::store::LogQueue::bounded(1);
    let app = crate::http::router(crate::http::App {
        keys: Arc::new(crate::http::Keys::new(c)),
        sender,
        logs,
    });
    let fixture = |name: &str| {
        std::fs::read(format!(
            "{}/e2e/fixtures/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    };
    let send = |req: Request<Body>| {
        let app = app.clone();
        async move {
            let r = app.oneshot(req).await.unwrap();
            let status = r.status().as_u16();
            let ty = r
                .headers()
                .get("content-type")
                .map(|v| v.to_str().unwrap().to_owned());
            let body = to_bytes(r.into_body(), 10_000).await.unwrap().to_vec();
            (status, ty, body)
        }
    };

    let logtail = |auth: &str| {
        Request::post("/")
            .header("authorization", auth)
            .header("content-type", "application/msgpack")
            .header("content-encoding", "gzip")
            .body(Body::from(fixture("logtail-pino/1.body")))
            .unwrap()
    };
    assert_eq!(send(logtail("Bearer key-b")).await.0, 202);
    let batch = logs_rx.try_recv().unwrap();
    assert_eq!(batch.len(), 3);
    assert_eq!(
        (batch[0].project_id, batch[0].message.as_str()),
        (2, "request completed")
    );
    assert_eq!(send(logtail("Bearer nope")).await.0, 403);
    assert_eq!(send(logtail("Basic key-b")).await.0, 401);
    let (status, _, _) = send(
        Request::post("/")
            .header("authorization", "Bearer key-a")
            .header("content-type", "application/json")
            .body(Body::from("\"text\""))
            .unwrap(),
    )
    .await;
    assert_eq!(status, 406);

    let otlp = |ty: &str, body: Vec<u8>| {
        Request::post("/v1/logs")
            .header("authorization", "Bearer key-a")
            .header("content-type", ty)
            .body(Body::from(body))
            .unwrap()
    };
    let (status, ty, body) =
        send(otlp("application/x-protobuf", fixture("otlp-proto/1.body"))).await;
    assert_eq!(
        (status, ty.as_deref(), body.as_slice()),
        (200, Some("application/x-protobuf"), &b""[..])
    );
    let batch = logs_rx.try_recv().unwrap();
    assert_eq!(
        (batch[0].project_id, batch[0].message.as_str()),
        (1, "GET /cart 200")
    );
    let (status, ty, body) = send(otlp("application/json", fixture("otlp-json/2.body"))).await;
    assert_eq!(
        (status, ty.as_deref(), body.as_slice()),
        (200, Some("application/json"), &b"{}"[..])
    );
    assert_eq!(
        logs_rx.try_recv().unwrap()[0].level,
        crate::logs::LogLevel::Error
    );

    let (status, ty, body) = send(otlp("application/json", b"{\"resourceLogs\":7}".to_vec())).await;
    assert_eq!((status, ty.as_deref()), (400, Some("application/json")));
    let status_body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(status_body["code"], 3);
    assert_eq!(send(otlp("text/plain", b"x".to_vec())).await.0, 415);
    let (status, ty, body) = send(
        Request::post("/v1/logs")
            .header("authorization", "Bearer wrong")
            .header("content-type", "application/x-protobuf")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(
        (status, ty.as_deref()),
        (403, Some("application/x-protobuf"))
    );
    assert_eq!(&body[..2], b"\x08\x03");

    let (status, _, _) = send(
        Request::post("/api/1/envelope/?sentry_key=key-a")
            .body(Body::from(fixture("sentry-node-10-logs/1.body")))
            .unwrap(),
    )
    .await;
    assert_eq!(status, 200);
    let batch = logs_rx.try_recv().unwrap();
    let messages: Vec<_> = batch.iter().map(|r| r.message.as_str()).collect();
    assert_eq!(
        messages,
        [
            "Order placed",
            "Cart c-77 is over the limit",
            "Payment provider timed out"
        ]
    );
    assert_eq!(
        send(
            Request::post("/api/2/envelope/?sentry_key=key-a")
                .body(Body::from(fixture("sentry-node-10-logs/1.body")))
                .unwrap(),
        )
        .await
        .0,
        403,
        "a key only opens its own project"
    );

    assert_eq!(
        send(otlp("application/json", fixture("otlp-json/2.body")))
            .await
            .0,
        200
    );
    let (status, _, _) = send(otlp("application/json", fixture("otlp-json/2.body"))).await;
    assert_eq!(status, 429, "log queue is full");
    let r = app
        .clone()
        .oneshot(otlp("application/json", fixture("otlp-json/2.body")))
        .await
        .unwrap();
    assert_eq!(r.headers()["retry-after"], "1");
    drop(logs_rx);
    let r = app
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri("/v1/logs")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        r.headers()["access-control-allow-headers"]
            .to_str()
            .unwrap()
            .contains("authorization")
    );
}
