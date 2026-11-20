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
use std::{
    sync::{Arc, mpsc},
    time::Duration,
};
use tokio::sync::Semaphore;
use tower::ServiceExt;

/// The router with projects `a` (id 1, key `key-a`) and `b` (id 2, key `key-b`),
/// both ingest queues one slot deep and one parsing slot.
struct Server {
    app: Router,
    errors: mpsc::SyncSender<store::Write>,
    _error_inbox: mpsc::Receiver<store::Write>,
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
    let (errors, error_inbox) = mpsc::sync_channel(1);
    let (logs, log_inbox) = LogQueue::bounded(1);
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
        _error_inbox: error_inbox,
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
