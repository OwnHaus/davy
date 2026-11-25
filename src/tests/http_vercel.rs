//! `POST /vercel/logs`, a Vercel Log Drain's custom endpoint.

use super::*;
use crate::logs::LogSource;

/// A drain batch in the shape of Vercel's documented example: a build log
/// with a project name, and a function log with ids and a proxy object.
const BATCH: &str = r#"[
  {"id":"1","deploymentId":"dpl_1","source":"build","host":"my-app-abc123.vercel.app","timestamp":1573817187330,"projectId":"prj_1","level":"info","message":"Build completed successfully","type":"stdout","projectName":"my-app"},
  {"id":"2","deploymentId":"dpl_1","source":"lambda","host":"my-app-abc123.vercel.app","timestamp":1573817250283,"projectId":"prj_1","level":"error","message":"API request failed","statusCode":500,"requestId":"643af4e3","traceId":"1b02cd14bb8642fd092bc23f54c7ffcd","spanId":"f24e8631bd11faa7","proxy":{"method":"GET","path":"/api/users?page=1","statusCode":500,"clientIp":"120.75.16.101"}}
]"#;

fn drain(auth: &str, body: impl Into<Body>) -> Request<Body> {
    post(
        "/vercel/logs",
        &[
            ("authorization", auth),
            ("content-type", "application/json"),
            (
                "user-agent",
                "VercelDrain/1.0 (+https://vercel.com/docs/drains)",
            ),
        ],
        body,
    )
}

#[tokio::test]
async fn a_drain_batch_is_queued_for_the_project_of_its_key() {
    let s = server();
    let r = s.send(drain("Bearer key-b", BATCH)).await;
    assert_eq!((r.status, r.body.as_slice()), (200, &b""[..]));
    let batch = s.logs.try_recv().unwrap();
    let got: Vec<_> = batch
        .iter()
        .map(|l| {
            (
                l.project_id,
                l.source,
                l.level,
                l.message.as_str(),
                l.service.as_deref(),
                l.trace_id.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            (
                2,
                LogSource::Vercel,
                LogLevel::Info,
                "Build completed successfully",
                Some("my-app"),
                None
            ),
            (
                2,
                LogSource::Vercel,
                LogLevel::Error,
                "API request failed",
                Some("prj_1"),
                Some("1b02cd14bb8642fd092bc23f54c7ffcd")
            ),
        ]
    );
    assert_eq!(batch[1].attributes["statusCode"], 500);
    assert_eq!(batch[1].attributes["proxy"]["path"], "/api/users?page=1");
}

#[tokio::test]
async fn a_drain_without_the_right_key_is_refused_unread() {
    let s = server();
    let unread = || {
        Body::from_stream(futures::stream::iter([Err::<Vec<u8>, _>(
            std::io::Error::other("the body was read"),
        )]))
    };
    for (auth, status) in [("", 401), ("Basic key-a", 401), ("Bearer nope", 403)] {
        let r = s.send(drain(auth, unread())).await;
        assert_eq!(r.status, status, "{auth:?}");
    }
    assert!(s.logs.try_recv().is_none(), "no log was queued");
}

#[tokio::test]
async fn a_drain_sends_ndjson_and_gzip() {
    let s = server();
    let ndjson = "{\"message\":\"one\",\"projectId\":\"prj_1\"}\n{\"message\":\"two\",\"projectId\":\"prj_1\"}\n";
    for (encoding, body) in [
        ("identity", ndjson.as_bytes().to_vec()),
        ("gzip", gzip(ndjson.as_bytes())),
    ] {
        let r = s
            .send(post(
                "/vercel/logs",
                &[
                    ("authorization", "Bearer key-a"),
                    ("content-type", "application/x-ndjson"),
                    ("content-encoding", encoding),
                ],
                body,
            ))
            .await;
        assert_eq!(r.status, 200, "{encoding}");
        let messages: Vec<_> = s
            .logs
            .try_recv()
            .unwrap()
            .iter()
            .map(|l| l.message.clone())
            .collect();
        assert_eq!(messages, ["one", "two"], "{encoding}");
    }
}

#[tokio::test]
async fn a_drain_body_that_is_not_json_is_400() {
    let s = server();
    for body in ["{\"message\":", "<html>", "5"] {
        let r = s.send(drain("Bearer key-a", body)).await;
        assert_eq!(r.status, 400, "{body}");
    }
    assert!(s.logs.try_recv().is_none(), "no log was queued");
}

#[tokio::test]
async fn a_drain_body_over_the_limits_is_413() {
    let s = server();
    let r = s
        .send(drain("Bearer key-a", "{\"message\":\"a\"}\n".repeat(4)))
        .await;
    assert_eq!(r.status, 413, "more records than the queue holds");
    let r = s
        .send(post(
            "/vercel/logs",
            &[
                ("authorization", "Bearer key-a"),
                ("content-encoding", "gzip"),
            ],
            gzip(&vec![b' '; 65 << 20]),
        ))
        .await;
    assert_eq!(r.status, 413, "inflates past 64 MiB");
    let objects = format!("[{}{{}}]", "{},".repeat(100_000));
    let r = s.send(drain("Bearer key-a", objects)).await;
    assert_eq!(
        (r.status, String::from_utf8(r.body).unwrap()),
        (413, TOO_MUCH.into()),
        "priced before it is parsed"
    );
    assert!(s.logs.try_recv().is_none(), "no log was queued");
}

#[tokio::test]
async fn a_drain_client_ip_is_scrubbed_by_default() {
    let s = server();
    assert_eq!(s.send(drain("Bearer key-a", BATCH)).await.status, 200);
    let batch = s.logs.try_recv().unwrap();
    assert_eq!(batch[1].attributes["proxy"]["clientIp"], "[Filtered: ip]");
}

#[tokio::test]
async fn a_drain_batch_counts_as_logs() {
    let s = server();
    assert_eq!(s.send(drain("Bearer key-a", BATCH)).await.status, 200);
    let counts: Vec<_> = s
        .usage
        .take()
        .into_iter()
        .map(|r| (r.project, r.signal.name(), r.count.records, r.count.bytes))
        .collect();
    let bytes = u64::try_from(BATCH.len()).unwrap();
    assert_eq!(counts, [(1, "logs", 2, bytes)]);
}

/// The `x-vercel-signature` of `body` under `secret`.
fn sign(secret: &str, body: &[u8]) -> String {
    use hmac::{Hmac, KeyInit as _, Mac as _};
    let mut mac = Hmac::<sha1::Sha1>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(body);
    crate::text::hex(&mac.finalize().into_bytes())
}

fn signed(signature: Option<&str>, body: impl Into<Body>) -> Request<Body> {
    let mut req = drain("Bearer key-c", body);
    if let Some(signature) = signature {
        req.headers_mut()
            .insert("x-vercel-signature", signature.parse().unwrap());
    }
    req
}

#[tokio::test]
async fn a_drain_signed_with_the_project_secret_is_queued() {
    let s = server();
    let signature = sign(DRAIN_SECRET, BATCH.as_bytes());
    let r = s.send(signed(Some(&signature), BATCH)).await;
    assert_eq!(r.status, 200);
    let batch = s.logs.try_recv().unwrap();
    assert_eq!((batch.len(), batch[0].project_id), (2, 3));
}

#[tokio::test]
async fn a_drain_signed_wrong_or_not_at_all_is_403() {
    let s = server();
    let other = sign("another-secret", BATCH.as_bytes());
    let altered = sign(DRAIN_SECRET, b"[]");
    for (signature, why) in [
        (None, "missing x-vercel-signature"),
        (Some("not-hex"), "invalid x-vercel-signature"),
        (Some(other.as_str()), "invalid x-vercel-signature"),
        (Some(altered.as_str()), "invalid x-vercel-signature"),
    ] {
        let r = s.send(signed(signature, BATCH)).await;
        assert_eq!(
            (r.status, String::from_utf8(r.body).unwrap()),
            (403, why.to_owned()),
            "{signature:?}"
        );
    }
    assert!(s.logs.try_recv().is_none(), "no log was queued");
}

#[tokio::test]
async fn a_gzip_drain_is_signed_as_sent_not_as_decompressed() {
    let s = server();
    let wire = gzip(BATCH.as_bytes());
    for (signature, status) in [
        (sign(DRAIN_SECRET, BATCH.as_bytes()), 403),
        (sign(DRAIN_SECRET, &wire), 200),
    ] {
        let mut req = signed(Some(&signature), wire.clone());
        req.headers_mut()
            .insert("content-encoding", "gzip".parse().unwrap());
        assert_eq!(s.send(req).await.status, status);
    }
    let batch = s.logs.try_recv().unwrap();
    assert_eq!((batch.len(), batch[0].project_id), (2, 3));
    assert!(s.logs.try_recv().is_none(), "one batch was queued");
}

#[tokio::test]
async fn a_drain_without_a_signature_header_is_refused_unread() {
    let s = server();
    let unread = Body::from_stream(futures::stream::iter([Err::<Vec<u8>, _>(
        std::io::Error::other("the body was read"),
    )]));
    let r = s.send(signed(None, unread)).await;
    assert_eq!(r.status, 403);
}

#[tokio::test]
async fn a_project_without_a_secret_needs_no_signature() {
    let s = server();
    let plain = drain("Bearer key-a", BATCH);
    let mut with_header = drain("Bearer key-a", BATCH);
    with_header
        .headers_mut()
        .insert("x-vercel-signature", "0".repeat(40).parse().unwrap());
    for req in [plain, with_header] {
        assert_eq!(s.send(req).await.status, 200);
        assert_eq!(s.logs.try_recv().unwrap().len(), 2);
    }
}
