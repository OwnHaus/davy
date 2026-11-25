use super::memory;
use crate::test_support::fixture;
use crate::{
    http::{App, Keys, RATE, body::Budget, router},
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
use std::{sync::Arc, time::Duration};
use tower::ServiceExt;

const TOKEN: &str = "0123456789abcdef0123456789abcdef";
/// The Vercel drain secret of project `c`.
const DRAIN_SECRET: &str = "drain-secret-of-c";

/// The router with projects `a` (id 1, key `key-a`), `b` (id 2, key `key-b`)
/// and `c` (id 3, key `key-c`, Vercel drain secret [`DRAIN_SECRET`]), a
/// heartbeat monitor (id 1, token [`TOKEN`]) in `a`, both ingest queues one
/// slot deep and [`BUDGET`] for bodies.
struct Server {
    app: Router,
    errors: store::Sender,
    error_inbox: crossbeam_channel::Receiver<store::Write>,
    logs: LogInbox,
    budget: Budget,
    usage: crate::usage::Counter,
}

const BUDGET: usize = 32 << 20;

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
    store::create_project(&c, "c").unwrap();
    c.execute("UPDATE projects SET public_key='key-' || slug", [])
        .unwrap();
    c.execute(
        "UPDATE projects SET vercel_secret=?1 WHERE slug='c'",
        [DRAIN_SECRET],
    )
    .unwrap();
    c.execute(
        &format!(
            "INSERT INTO monitors(project_id,name,kind,heartbeat_token,every_s,grace_s,state,created_at)
             VALUES (1,'Backup','heartbeat','{TOKEN}',3600,300,'pending',0)"
        ),
        [],
    )
    .unwrap();
    let (errors, error_inbox) = crossbeam_channel::bounded(1);
    let (logs, log_inbox) = LogQueue::bounded(3, 1 << 20);
    let budget = Budget::new(BUDGET);
    let usage = crate::usage::Counter::default();
    let app = router(
        App {
            budget: budget.clone(),
            keys: Arc::new(Keys::new(c)),
            sender: errors.clone(),
            logs,
            sourcemaps: Arc::new(crate::sourcemaps::Store::open(std::path::Path::new(
                "target/no-sourcemaps",
            ))),
            usage: usage.clone(),
        },
        Router::new().route("/admin", axum::routing::get(|| async { "ok" })),
    );
    Server {
        app,
        errors,
        error_inbox,
        logs: log_inbox,
        budget,
        usage,
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
async fn every_ingest_route_is_429_while_the_budget_is_spent() {
    let s = server();
    let mut taken = s.budget.hold();
    taken.take(BUDGET).unwrap();
    for req in [
        post("/api/1/store/?sentry_key=key-a", &[], "{}"),
        post("/api/1/envelope/?sentry_key=key-a", &[], SESSION),
        post("/", &[("authorization", "Bearer key-a")], "[]"),
        otlp("application/json", "{}"),
        post("/vercel/logs", &[("authorization", "Bearer key-a")], "[]"),
    ] {
        let uri = req.uri().to_string();
        let r = s.send(req).await;
        assert_eq!(r.status, 429, "{uri}");
        assert_eq!(r.headers["retry-after"], "1", "{uri}");
    }
}

#[tokio::test]
async fn a_queued_event_keeps_its_memory_after_the_client_hangs_up() {
    let s = server();
    let req = post("/api/1/store/?sentry_key=key-a", &[], r#"{"message":"m"}"#);
    let hung_up = tokio::time::timeout(Duration::from_millis(500), s.send(req)).await;
    assert!(
        hung_up.is_err(),
        "the event waits for a writer this test does not run"
    );
    assert_eq!(s.budget.hold().take(BUDGET).unwrap_err().0, 429);
    drop(s.error_inbox.try_recv().unwrap());
    s.budget.hold().take(BUDGET).unwrap();
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
        post(
            "/vercel/logs",
            &[("authorization", "Bearer key-a")],
            stalled(),
        ),
    ] {
        let uri = req.uri().to_string();
        assert_eq!(s.send(req).await.status, 408, "{uri}");
    }
    let r = s
        .send(post("/api/1/envelope/?sentry_key=key-a", &[], SESSION))
        .await;
    assert_eq!(r.status, 200, "a stalled body gives back what it held");
}

#[tokio::test]
async fn a_refused_envelope_is_not_read_past_its_header_line() {
    use futures::StreamExt as _;
    let chunks = futures::stream::iter(0..2).map(|i| {
        assert_eq!(i, 0, "the body was read past the refused header line");
        Ok::<_, std::io::Error>(
            b"{\"dsn\":\"http://wrong@h/1\"}\n{\"type\":\"session\"}\n".to_vec(),
        )
    });
    let r = server()
        .send(post("/api/1/envelope/", &[], Body::from_stream(chunks)))
        .await;
    assert_eq!(r.status, 403);
}

fn gzip(body: &[u8]) -> Vec<u8> {
    use std::io::Write as _;
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(body).unwrap();
    gz.finish().unwrap()
}

const TOO_MUCH: &str = "the body needs more memory than one request may use";

#[tokio::test]
async fn a_small_body_of_many_values_is_413_before_it_is_parsed() {
    let objects = format!("[{}{{}}]", "{},".repeat(100_000));
    let body = gzip(objects.as_bytes());
    assert!(body.len() < 4096, "{} bytes", body.len());
    let r = server()
        .send(post(
            "/",
            &[
                ("authorization", "Bearer key-a"),
                ("content-type", "application/json"),
                ("content-encoding", "gzip"),
            ],
            body,
        ))
        .await;
    assert_eq!(
        (r.status, String::from_utf8(r.body).unwrap()),
        (413, TOO_MUCH.into())
    );
}

#[tokio::test]
async fn records_that_copy_a_large_resource_are_413() {
    use opentelemetry_proto::tonic::{
        collector::logs::v1::ExportLogsServiceRequest,
        common::v1::{AnyValue, KeyValue, any_value::Value},
        logs::v1::{LogRecord, ResourceLogs, ScopeLogs},
        resource::v1::Resource,
    };
    use prost::Message as _;
    let attributes = (0..200)
        .map(|i| KeyValue {
            key: format!("attribute.{i}"),
            value: Some(AnyValue {
                value: Some(Value::StringValue("value".into())),
            }),
            ..KeyValue::default()
        })
        .collect();
    let request = ExportLogsServiceRequest {
        resource_logs: vec![ResourceLogs {
            resource: Some(Resource {
                attributes,
                ..Resource::default()
            }),
            scope_logs: vec![ScopeLogs {
                log_records: vec![LogRecord::default(); 5_000],
                ..ScopeLogs::default()
            }],
            ..ResourceLogs::default()
        }],
    };
    let r = server()
        .send(otlp("application/x-protobuf", request.encode_to_vec()))
        .await;
    assert_eq!(r.status, 413);
    assert!(
        r.body
            .windows(TOO_MUCH.len())
            .any(|w| w == TOO_MUCH.as_bytes()),
        "the status says why"
    );
}

#[tokio::test]
async fn a_request_of_more_than_10000_records_is_413() {
    let s = server();
    let send = |n: usize| {
        let objects = format!("[{}{{}}]", "{},".repeat(n - 1));
        post("/", &[("authorization", "Bearer key-a")], objects)
    };
    let r = s.send(send(10_001)).await;
    assert_eq!(
        (r.status, String::from_utf8(r.body).unwrap()),
        (413, "more than 10000 log records in one request".into())
    );
    let r = s.send(send(10_000)).await;
    assert_eq!(
        (r.status, String::from_utf8(r.body).unwrap()),
        (
            413,
            "log batch is larger than the whole ingest queue".into()
        ),
        "10000 records pass the count and reach the queue"
    );
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
            held: Budget::new(0).hold(),
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

#[path = "http_heartbeat.rs"]
mod heartbeat;
#[path = "http_items.rs"]
mod items;
#[path = "scrub_http.rs"]
mod scrub_http;
#[path = "http_surface.rs"]
mod surface;
#[path = "http_usage.rs"]
mod usage_http;
#[path = "http_vercel.rs"]
mod vercel;
