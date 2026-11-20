use super::memory;
use crate::test_support::fixture;
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
use std::{
    sync::{Arc, mpsc},
    time::Duration,
};
use tokio::sync::Semaphore;
use tower::ServiceExt;

const TOKEN: &str = "0123456789abcdef0123456789abcdef";

/// The router with projects `a` (id 1, key `key-a`) and `b` (id 2, key `key-b`),
/// a heartbeat monitor (id 1, token [`TOKEN`]) in `a`, both ingest queues one
/// slot deep and one parsing slot.
struct Server {
    app: Router,
    errors: mpsc::SyncSender<store::Write>,
    error_inbox: mpsc::Receiver<store::Write>,
    logs: LogInbox,
    parsing: Arc<Semaphore>,
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
    c.execute(
        &format!(
            "INSERT INTO monitors(project_id,name,kind,heartbeat_token,every_s,grace_s,state,created_at)
             VALUES (1,'Backup','heartbeat','{TOKEN}',3600,300,'pending',0)"
        ),
        [],
    )
    .unwrap();
    let (errors, error_inbox) = mpsc::sync_channel(1);
    let (logs, log_inbox) = LogQueue::bounded(3, 1 << 20);
    let parsing = Arc::new(Semaphore::new(1));
    let app = router(App {
        parsing: Arc::clone(&parsing),
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
        error_inbox,
        logs: log_inbox,
        parsing,
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
async fn a_key_inside_the_envelope_is_checked_before_the_items_are_read() {
    let s = server();
    for (body, status) in [
        (
            "{\"dsn\":\"http://key-a@h/1\"}\n{\"type\":\"session\"}\n{}",
            200,
        ),
        (
            "{\"dsn\":\"http://key-b@h/2\"}\n{\"type\":\"session\"}\n{}",
            403,
        ),
        ("{\"dsn\":\"http://wrong@h/1\"}\n{broken", 403),
        ("{}\n{broken", 401),
    ] {
        let r = s.send(post("/api/1/envelope/", &[], body)).await;
        assert_eq!(r.status, status, "{body}");
    }
    let long_header = format!("{{\"pad\":\"{}\"}}\n", "x".repeat(70_000));
    let r = s.send(post("/api/1/envelope/", &[], long_header)).await;
    assert_eq!(r.status, 400, "a header line over 64 KiB");
}

#[tokio::test]
async fn an_anonymous_body_is_not_decompressed() {
    use std::io::Write as _;
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(b"{\"dsn\":\"http://wrong@h/1\"}\n").unwrap();
    gz.write_all(&vec![b' '; 201 * 1024 * 1024]).unwrap();
    let bomb = gz.finish().unwrap();
    let s = server();
    let r = s
        .send(post(
            "/api/1/envelope/",
            &[("content-encoding", "gzip")],
            bomb,
        ))
        .await;
    assert_eq!(
        r.status, 403,
        "the header key fails before the rest inflates"
    );
    let r = s
        .send(post(
            "/api/1/store/",
            &[("content-encoding", "gzip")],
            "not gzip",
        ))
        .await;
    assert_eq!(r.status, 401, "/store/ has no header to carry a key");
}

#[tokio::test]
async fn every_ingest_route_is_429_while_the_parsing_slots_are_taken() {
    let s = server();
    let _taken = Arc::clone(&s.parsing).try_acquire_owned().unwrap();
    for req in [
        post("/api/1/store/?sentry_key=key-a", &[], "{}"),
        post("/api/1/envelope/?sentry_key=key-a", &[], SESSION),
        post("/", &[("authorization", "Bearer key-a")], "[]"),
        otlp("application/json", "{}"),
    ] {
        let uri = req.uri().to_string();
        let r = s.send(req).await;
        assert_eq!(r.status, 429, "{uri}");
        assert_eq!(r.headers["retry-after"], "1", "{uri}");
    }
}

#[tokio::test(start_paused = true)]
async fn a_body_that_stalls_is_408() {
    let stalled =
        || Body::from_stream(futures::stream::pending::<Result<Vec<u8>, std::io::Error>>());
    let s = server();
    for req in [
        post("/api/1/envelope/?sentry_key=key-a", &[], stalled()),
        post("/", &[("authorization", "Bearer key-a")], stalled()),
        otlp("application/json", stalled()),
    ] {
        let uri = req.uri().to_string();
        assert_eq!(s.send(req).await.status, 408, "{uri}");
    }
    let r = s
        .send(post("/api/1/envelope/?sentry_key=key-a", &[], SESSION))
        .await;
    assert_eq!(r.status, 200, "stalled bodies hold no parsing slot");
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

fn dummy_write() -> store::Write {
    let (done, _) = tokio::sync::oneshot::channel();
    store::Write {
        job: store::Job::Ingest {
            project: 1,
            events: vec![],
        },
        done,
    }
}

#[tokio::test]
async fn full_error_queue_is_429() {
    let s = server();
    s.errors.try_send(dummy_write()).unwrap();
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
async fn items_over_one_mib_are_dropped_before_parsing() {
    let s = server();
    let big = format!("{{\"message\":\"{}\"}}", "x".repeat(1024 * 1024));
    let logs = format!(
        "{{\"items\":[{{\"timestamp\":1,\"level\":\"info\",\"body\":\"{}\"}}]}}",
        "x".repeat(1024 * 1024)
    );
    for (uri, body) in [
        ("/api/1/store/?sentry_key=key-a", big.clone()),
        (
            "/api/1/envelope/?sentry_key=key-a",
            format!("{{}}\n{{\"type\":\"event\"}}\n{big}"),
        ),
        (
            "/api/1/envelope/?sentry_key=key-a",
            format!("{{}}\n{{\"type\":\"log\"}}\n{logs}"),
        ),
    ] {
        // A queued event waits for a writer this test does not run.
        let r = tokio::time::timeout(Duration::from_secs(5), s.send(post(uri, &[], body)))
            .await
            .expect("answered without queueing an event");
        assert_eq!(r.status, 200, "{uri}");
    }
    assert!(
        s.errors.try_send(dummy_write()).is_ok(),
        "no event was queued"
    );
    assert!(s.logs.try_recv().is_none(), "no log was queued");
}

#[tokio::test]
async fn an_envelope_stores_only_its_first_event() {
    let s = server();
    let body = "{}\n{\"type\":\"event\"}\n{\"message\":\"first\"}\n{\"type\":\"event\"}\n{\"message\":\"second\"}\n";
    let queued = async {
        loop {
            if let Ok(write) = s.error_inbox.try_recv() {
                let store::Job::Ingest { events, .. } = write.job else {
                    return Vec::new();
                };
                return events.into_iter().map(|e| e.message).collect::<Vec<_>>();
            }
            tokio::task::yield_now().await;
        }
    };
    let (_, messages) = tokio::join!(
        s.send(post("/api/1/envelope/?sentry_key=key-a", &[], body)),
        queued
    );
    assert_eq!(messages, [Some("first".to_owned())]);
}

#[tokio::test]
async fn full_log_queue_is_429() {
    let s = server();
    let body = fixture("otlp-json/2.body");
    for _ in 0..3 {
        let r = s.send(otlp("application/json", body.clone())).await;
        assert_eq!(r.status, 200);
    }
    let r = s.send(otlp("application/json", body)).await;
    assert_eq!(r.status, 429);
    assert_eq!(r.headers["retry-after"], "1");
}

#[tokio::test]
async fn a_log_batch_larger_than_the_queue_is_413() {
    let s = server();
    let r = s
        .send(post(
            "/",
            &[
                ("authorization", "Bearer key-a"),
                ("content-type", "application/x-ndjson"),
            ],
            "{\"message\":\"a\"}\n".repeat(4),
        ))
        .await;
    assert_eq!(r.status, 413);
    assert!(s.logs.try_recv().is_none(), "no log was queued");
}

fn request(method: &str, uri: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn a_heartbeat_ping_queues_the_monitor_it_names() {
    let s = server();
    for (method, path, failed) in [
        ("GET", String::new(), false),
        ("HEAD", String::new(), false),
        ("POST", String::new(), false),
        ("GET", "/fail".to_owned(), true),
        ("POST", "/fail".to_owned(), true),
    ] {
        let uri = format!("/api/heartbeat/{TOKEN}{path}");
        let queued = async {
            loop {
                if let Ok(write) = s.error_inbox.try_recv() {
                    let store::Job::Ping {
                        monitor, failed, ..
                    } = write.job
                    else {
                        return None;
                    };
                    write.done.send(Ok(())).unwrap();
                    return Some((monitor, failed));
                }
                tokio::task::yield_now().await;
            }
        };
        let (r, ping) = tokio::join!(s.send(request(method, &uri)), queued);
        assert_eq!(r.status, 200, "{method} {uri}");
        let body = if method == "HEAD" { "" } else { "ok" };
        assert_eq!(r.body, body.as_bytes(), "{method} {uri}");
        assert_eq!(ping, Some((1, failed)), "{method} {uri}");
    }
}

#[tokio::test]
async fn a_heartbeat_with_an_unknown_token_is_404() {
    let s = server();
    for uri in [
        "/api/heartbeat/ffffffffffffffffffffffffffffffff",
        "/api/heartbeat/ffffffffffffffffffffffffffffffff/fail",
        "/api/heartbeat/0123456789ABCDEF0123456789ABCDEF",
        "/api/heartbeat/not-a-token",
        "/api/heartbeat/",
    ] {
        let r = s.send(request("GET", uri)).await;
        assert_eq!(r.status, 404, "{uri}");
    }
    assert!(
        s.errors.try_send(dummy_write()).is_ok(),
        "nothing was queued"
    );
}

#[tokio::test]
async fn a_heartbeat_ping_to_a_full_queue_is_429() {
    let s = server();
    s.errors.try_send(dummy_write()).unwrap();
    let r = s
        .send(request("GET", &format!("/api/heartbeat/{TOKEN}")))
        .await;
    assert_eq!(r.status, 429);
    assert_eq!(r.headers["retry-after"], "1");
}
