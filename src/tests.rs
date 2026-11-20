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
    assert_eq!(e.timestamp, 1_577_836_800_000);
    assert_eq!(e.message.as_deref(), Some("hi"));
    assert_eq!(e.message_template.as_deref(), Some("template"));
    assert!(ErrorEvent::parse(json!({"event_id":"bad"})).is_err());
    let e = event(
        json!({"level":"warn","timestamp":"2020-01-01T00:00:00","fingerprint":["a",1,null],"exception":{"values":[{"type":"E","value":7,"stacktrace":{"frames":[{"function":"f","lineno":-1}]}}]}}),
    );
    assert_eq!(e.level, Level::Warning);
    assert_eq!(e.timestamp, 1_577_836_800_000);
    assert_eq!(e.fingerprint, Some(vec!["a".to_owned(), "1".to_owned()]));
    assert_eq!(e.exceptions[0].value, None);
    assert_eq!(e.exceptions[0].frames[0].function.as_deref(), Some("f"));
    assert_eq!(e.exceptions[0].frames[0].lineno, None);
    let e = event(json!({"timestamp":"yesterday","level":7}));
    assert_eq!(e.level, Level::Error);
    assert!(e.timestamp > 1_577_836_800_000);
}
fn memory() -> rusqlite::Connection {
    let mut c = rusqlite::Connection::open_in_memory().unwrap();
    store::init(&mut c).unwrap();
    c
}
#[test]
fn a_resolved_issue_reopens_only_for_a_release_that_can_contain_the_bug() {
    use store::IssueStatus::{Ignored, Resolved, Unresolved};
    let fixed_in = |release: Option<&str>| Resolved {
        in_release: release.map(str::to_owned),
        at: 100,
    };
    let cases = [
        (fixed_in(Some("v1")), None, Some(500)),
        (fixed_in(Some("v1")), Some(("v1", 50)), Some(500)),
        (fixed_in(Some("v1")), Some(("v0", 50)), None),
        (fixed_in(Some("v1")), Some(("v2", 150)), Some(500)),
        (fixed_in(None), Some(("v0", 50)), None),
        (fixed_in(None), Some(("v2", 150)), Some(500)),
        (Unresolved, None, None),
        (Ignored, Some(("v2", 150)), None),
    ];
    for (mut status, release, reopened) in cases {
        assert_eq!(status.on_event(release, 500), reopened, "{release:?}");
    }
}
#[test]
fn resolving_waits_for_the_next_release() {
    let c = memory();
    store::create_project(&c, "releases").unwrap();
    let in_release = |release: &str, received_at| ErrorEvent {
        received_at,
        ..event(json!({"message":"boom","release":release}))
    };
    let status = || {
        c.query_row("SELECT status FROM issues", [], |r| r.get::<_, String>(0))
            .unwrap()
    };
    store::insert(&c, 1, &in_release("v1", 1000)).unwrap();
    assert!(store::resolve(&c, 1, None, 2000).unwrap());
    store::insert(&c, 1, &in_release("v1", 3000)).unwrap();
    assert_eq!(status(), "resolved");
    store::insert(&c, 1, &in_release("v2", 3000)).unwrap();
    assert_eq!(status(), "unresolved");
    assert!(!store::resolve(&c, 99, None, 0).unwrap());
}
#[test]
fn migration_backfills_releases_from_old_events() {
    let mut c = memory();
    store::create_project(&c, "old").unwrap();
    let e = event(json!({"message":"boom","release":"v1"}));
    store::insert(&c, 1, &e).unwrap();
    c.execute_batch("DROP TABLE releases; PRAGMA user_version=0;")
        .unwrap();
    store::init(&mut c).unwrap();
    store::init(&mut c).unwrap();
    let row: (String, i64) = c
        .query_row("SELECT version,first_seen FROM releases", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(row, ("v1".into(), e.received_at));
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
    assert_eq!(
        store::payload(&c, 1).unwrap().unwrap()["message"],
        "failure 3"
    );
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

mod http {
    use super::memory;
    use crate::{
        http::{App, Keys, RATE, router},
        logs::{
            LogLevel,
            store::{LogInbox, LogQueue},
        },
        store,
    };
    use axum::{
        Router,
        body::{Body, to_bytes},
        http::{HeaderMap, Request, header},
    };
    use std::sync::{Arc, mpsc};
    use tower::ServiceExt;

    /// The router with projects `a` (id 1, key `key-a`) and `b` (id 2, key `key-b`)
    /// and both ingest queues one slot deep.
    struct Server {
        app: Router,
        errors: mpsc::SyncSender<store::Write>,
        _error_inbox: mpsc::Receiver<store::Write>,
        logs: LogInbox,
    }

    struct Reply {
        status: u16,
        headers: HeaderMap,
        body: Vec<u8>,
    }

    impl Reply {
        fn content_type(&self) -> &str {
            self.headers
                .get(header::CONTENT_TYPE)
                .map_or("", |v| v.to_str().unwrap())
        }
    }

    fn server() -> Server {
        let c = memory();
        store::create_project(&c, "a").unwrap();
        store::create_project(&c, "b").unwrap();
        c.execute("UPDATE projects SET public_key='key-' || slug", [])
            .unwrap();
        let (errors, error_inbox) = mpsc::sync_channel(1);
        let (logs, log_inbox) = LogQueue::bounded(1);
        let app = router(App {
            keys: Arc::new(Keys::new(c)),
            sender: errors.clone(),
            logs,
            sourcemaps: Arc::new(crate::sourcemaps::Store::open(std::path::Path::new(
                "target/no-sourcemaps",
            ))),
        });
        Server {
            app,
            errors,
            _error_inbox: error_inbox,
            logs: log_inbox,
        }
    }

    impl Server {
        async fn send(&self, req: Request<Body>) -> Reply {
            let r = self.app.clone().oneshot(req).await.unwrap();
            let status = r.status().as_u16();
            let headers = r.headers().clone();
            let body = to_bytes(r.into_body(), 10_000).await.unwrap().to_vec();
            Reply {
                status,
                headers,
                body,
            }
        }
    }

    fn post(uri: &str, headers: &[(&str, &str)], body: impl Into<Body>) -> Request<Body> {
        headers
            .iter()
            .fold(Request::post(uri), |r, (k, v)| r.header(*k, *v))
            .body(body.into())
            .unwrap()
    }

    fn otlp(content_type: &str, body: impl Into<Body>) -> Request<Body> {
        post(
            "/v1/logs",
            &[
                ("authorization", "Bearer key-a"),
                ("content-type", content_type),
            ],
            body,
        )
    }

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!(
            "{}/e2e/fixtures/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    const SESSION: &str = "{}\n{\"type\":\"session\"}\n{}";

    #[tokio::test]
    async fn envelope_needs_the_project_key() {
        let s = server();
        for (uri, status) in [
            ("/api/1/envelope/", 401),
            ("/api/1/envelope/?sentry_key=wrong", 403),
            ("/api/2/envelope/?sentry_key=key-a", 403),
        ] {
            let r = s.send(post(uri, &[], SESSION)).await;
            assert_eq!(r.status, status, "{uri}");
            assert_eq!(r.headers["x-sentry-rate-limits"], RATE);
        }
        let r = s
            .send(post(
                "/api/1/envelope/?sentry_key=wrong",
                &[("content-encoding", "gzip")],
                "not gzip",
            ))
            .await;
        assert_eq!(r.status, 403, "key is checked before the body is decoded");
    }

    #[tokio::test]
    async fn envelope_accepts_unknown_item_types() {
        let r = server()
            .send(post(
                "/api/1/envelope/?sentry_key=key-a",
                &[],
                "{}\n{\"type\":\"future\"}\n{}",
            ))
            .await;
        assert_eq!(r.status, 200);
        let reply: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
        assert_eq!(reply["id"].as_str().unwrap().len(), 32);
    }

    #[tokio::test]
    async fn full_error_queue_is_429() {
        let s = server();
        let (done, _) = tokio::sync::oneshot::channel();
        s.errors
            .try_send(store::Write {
                job: store::Job::Ingest {
                    project: 1,
                    events: vec![],
                },
                done,
            })
            .unwrap();
        let r = s
            .send(post(
                "/api/1/store/?sentry_key=key-a",
                &[],
                "{\"message\":\"test\"}",
            ))
            .await;
        assert_eq!(r.status, 429);
        assert_eq!(r.headers["retry-after"], "1");
    }

    #[tokio::test]
    async fn preflight_allows_browser_sdks() {
        let s = server();
        for uri in ["/api/1/store/", "/v1/logs"] {
            let r = s
                .send(
                    Request::builder()
                        .method("OPTIONS")
                        .uri(uri)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await;
            assert_eq!(r.status, 204, "{uri}");
            assert_eq!(r.headers["access-control-allow-origin"], "*");
            assert!(
                r.headers["access-control-allow-headers"]
                    .to_str()
                    .unwrap()
                    .contains("authorization")
            );
        }
    }

    #[tokio::test]
    async fn logtail_route() {
        let s = server();
        let logtail = |auth: &str| {
            post(
                "/",
                &[
                    ("authorization", auth),
                    ("content-type", "application/msgpack"),
                    ("content-encoding", "gzip"),
                ],
                fixture("logtail-pino/1.body"),
            )
        };
        assert_eq!(s.send(logtail("Bearer key-b")).await.status, 202);
        let batch = s.logs.try_recv().unwrap();
        assert_eq!(batch.len(), 3);
        assert_eq!(
            (batch[0].project_id, batch[0].message.as_str()),
            (2, "request completed")
        );
        assert_eq!(s.send(logtail("Bearer nope")).await.status, 403);
        assert_eq!(s.send(logtail("Basic key-b")).await.status, 401);
        let r = s
            .send(post(
                "/",
                &[
                    ("authorization", "Bearer key-a"),
                    ("content-type", "application/json"),
                ],
                "\"text\"",
            ))
            .await;
        assert_eq!(r.status, 406);
    }

    #[tokio::test]
    async fn otlp_route_answers_in_the_request_encoding() {
        let s = server();
        let r = s
            .send(otlp("application/x-protobuf", fixture("otlp-proto/1.body")))
            .await;
        assert_eq!(
            (r.status, r.content_type(), r.body.as_slice()),
            (200, "application/x-protobuf", &b""[..])
        );
        let batch = s.logs.try_recv().unwrap();
        assert_eq!(
            (batch[0].project_id, batch[0].message.as_str()),
            (1, "GET /cart 200")
        );

        let r = s
            .send(otlp("application/json", fixture("otlp-json/2.body")))
            .await;
        assert_eq!(
            (r.status, r.content_type(), r.body.as_slice()),
            (200, "application/json", &b"{}"[..])
        );
        assert_eq!(s.logs.try_recv().unwrap()[0].level, LogLevel::Error);

        let r = s
            .send(otlp("application/json", "{\"resourceLogs\":7}"))
            .await;
        assert_eq!((r.status, r.content_type()), (400, "application/json"));
        let status: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
        assert_eq!(status["code"], 3);
        assert_eq!(s.send(otlp("text/plain", "x")).await.status, 415);

        let r = s
            .send(post(
                "/v1/logs",
                &[
                    ("authorization", "Bearer wrong"),
                    ("content-type", "application/x-protobuf"),
                ],
                Body::empty(),
            ))
            .await;
        assert_eq!(
            (r.status, r.content_type()),
            (403, "application/x-protobuf")
        );
        assert_eq!(&r.body[..2], b"\x08\x03");
    }

    #[tokio::test]
    async fn sentry_log_items() {
        let s = server();
        let r = s
            .send(post(
                "/api/1/envelope/?sentry_key=key-a",
                &[],
                fixture("sentry-node-10-logs/1.body"),
            ))
            .await;
        assert_eq!(r.status, 200);
        let batch = s.logs.try_recv().unwrap();
        let messages: Vec<_> = batch.iter().map(|r| r.message.as_str()).collect();
        assert_eq!(
            messages,
            [
                "Order placed",
                "Cart c-77 is over the limit",
                "Payment provider timed out"
            ]
        );
    }

    #[tokio::test]
    async fn full_log_queue_is_429() {
        let s = server();
        let body = fixture("otlp-json/2.body");
        assert_eq!(
            s.send(otlp("application/json", body.clone())).await.status,
            200
        );
        let r = s.send(otlp("application/json", body)).await;
        assert_eq!(r.status, 429);
        assert_eq!(r.headers["retry-after"], "1");
    }
}
