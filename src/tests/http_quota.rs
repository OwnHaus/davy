//! A project past a daily quota: `429` with `Retry-After` until midnight
//! UTC and a rate limit entry per refused category, counted as
//! `davy_quota`, while other projects are taken as before.

use super::*;
use crate::{
    quota::{Refusal, Settings},
    time,
    usage::{Count, Signal},
};
use std::collections::HashMap;

const EVENT: &str = "{\"message\":\"boom\"}";
const OTLP_JSON: &str =
    r#"{"resourceLogs":[{"scopeLogs":[{"logRecords":[{"body":{"stringValue":"hi"}}]}]}]}"#;

/// Gives project `a` (id 1) a quota of one error event and one log byte,
/// and uses both up, out of reach of midnight UTC, which would reset them.
async fn over_quota(s: &Server) {
    crate::test_support::away_from_midnight().await;
    let gate = s.usage.gate();
    let quota = Settings {
        errors: Some(1),
        logs: Some(1),
        ..Settings::default()
    };
    gate.load(HashMap::from([(1, quota)]), time::now_ns());
    gate.used(1, Signal::Errors, Count::new(1, 1), time::now_ns());
    gate.used(1, Signal::Logs, Count::new(1, 1), time::now_ns());
}

/// The quota entries a `429` names, checked against its `Retry-After`,
/// which counts down to midnight UTC, at most 5 min.
fn quota_entries(r: &Reply) -> String {
    let retry: u64 = r.headers["retry-after"].to_str().unwrap().parse().unwrap();
    assert!((1..=300).contains(&retry), "{retry}");
    let limits = r.headers["x-sentry-rate-limits"].to_str().unwrap();
    let own = limits.strip_prefix(&format!("{RATE}, ")).unwrap();
    own.replace(&format!("{retry}:"), "S:")
}

/// Answers the next queued job and returns it.
async fn take(s: &Server) -> store::Job {
    loop {
        if let Ok(write) = s.error_inbox.try_recv() {
            drop(write.done.send(Ok(())));
            return write.job;
        }
        tokio::task::yield_now().await;
    }
}

fn refused(s: &Server) -> Vec<(i64, &'static str, &'static str, i64)> {
    let flush = s.usage.gate().take();
    flush
        .refused
        .into_iter()
        .map(|r: Refusal| (r.project, r.reason, r.category, r.quantity))
        .collect()
}

#[tokio::test]
async fn an_event_past_the_quota_is_429_and_another_project_is_taken() {
    let s = server();
    over_quota(&s).await;
    let r = s
        .send(post("/api/1/store/?sentry_key=key-a", &[], EVENT))
        .await;
    assert_eq!(r.status, 429);
    assert_eq!(
        quota_entries(&r),
        "S:error:project:davy_quota, S:log_item:project:davy_quota"
    );
    assert!(s.error_inbox.try_recv().is_err());
    let (r, job) = tokio::join!(
        s.send(post("/api/2/store/?sentry_key=key-b", &[], EVENT)),
        take(&s)
    );
    assert!(matches!(job, store::Job::Ingest { project: 2, .. }));
    assert_eq!(r.status, 200);
    assert_eq!(r.headers["x-sentry-rate-limits"], RATE);
    assert!(r.headers.get("retry-after").is_none());
    assert_eq!(refused(&s), [(1, "davy_quota", "error", 1)]);
}

#[tokio::test]
async fn a_wrong_key_is_refused_before_the_quota_and_not_counted() {
    let s = server();
    over_quota(&s).await;
    let r = s
        .send(post("/api/1/store/?sentry_key=key-b", &[], EVENT))
        .await;
    assert_eq!(r.status, 403);
    let r = s
        .send(post("/api/1/store/?sentry_key=nope", &[], EVENT))
        .await;
    assert_eq!(r.status, 403);
    assert_eq!(refused(&s), []);
}

#[tokio::test]
async fn an_envelope_keeps_what_no_quota_refuses() {
    let s = server();
    over_quota(&s).await;
    let session = r#"{"sid":"6e3a9a0bd0a14e4f8a6e1c2d3b4a5f60","init":true,"started":"2024-10-02T07:12:30Z","status":"ok","attrs":{"release":"web@3"}}"#;
    let body =
        format!("{{}}\n{{\"type\":\"event\"}}\n{EVENT}\n{{\"type\":\"session\"}}\n{session}\n");
    let (r, job) = tokio::join!(
        s.send(post("/api/1/envelope/?sentry_key=key-a", &[], body)),
        take(&s)
    );
    assert_eq!(r.status, 200);
    assert!(r.headers.get("retry-after").is_none());
    let limits = r.headers["x-sentry-rate-limits"].to_str().unwrap();
    assert!(limits.ends_with(":log_item:project:davy_quota"), "{limits}");
    assert!(matches!(job, store::Job::Items { project: 1, .. }));
    assert_eq!(refused(&s), [(1, "davy_quota", "error", 1)]);
}

#[tokio::test]
async fn an_envelope_with_nothing_left_is_429_and_counts_each_record() {
    let s = server();
    over_quota(&s).await;
    let r = s
        .send(post(
            "/api/1/envelope/?sentry_key=key-a",
            &[],
            fixture("sentry-node-10-logs/1.body"),
        ))
        .await;
    assert_eq!(r.status, 429);
    assert!(s.logs.try_recv().is_none());
    let events = format!("{{}}\n{{\"type\":\"event\"}}\n{EVENT}\n{{\"type\":\"event\"}}\n{EVENT}");
    let r = s
        .send(post("/api/1/envelope/?sentry_key=key-a", &[], events))
        .await;
    assert_eq!(r.status, 429);
    assert_eq!(
        refused(&s),
        [
            (1, "davy_quota", "error", 1),
            (1, "davy_quota", "log_item", 3)
        ]
    );
}

#[tokio::test]
async fn otlp_past_the_quota_is_429_with_a_status_body() {
    let s = server();
    over_quota(&s).await;
    let r = s.send(otlp("application/json", OTLP_JSON)).await;
    assert_eq!((r.status, r.content_type()), (429, "application/json"));
    assert_eq!(
        quota_entries(&r),
        "S:error:project:davy_quota, S:log_item:project:davy_quota"
    );
    let status: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
    assert_eq!(
        status,
        serde_json::json!({"code": 8, "message": "over the project's daily quota"})
    );
    assert!(s.logs.try_recv().is_none());
    let logtail = post(
        "/",
        &[
            ("authorization", "Bearer key-a"),
            ("content-type", "application/json"),
        ],
        "{\"message\":\"hi\"}",
    );
    assert_eq!(s.send(logtail).await.status, 429);
    let flush = s.usage.gate().take();
    assert_eq!(flush.refused, [], "records refused unread are not counted");
    let requests: Vec<_> = flush
        .requests
        .iter()
        .map(|r| (r.project, r.reason, r.quantity))
        .collect();
    assert_eq!(requests, [(1, "davy_quota", 2)]);
}

#[tokio::test]
async fn a_signed_drain_past_the_quota_is_200_and_counts_its_records_once_its_signature_matches() {
    use hmac::{Hmac, KeyInit as _, Mac as _};
    crate::test_support::away_from_midnight().await;
    let s = server();
    let gate = s.usage.gate();
    let quota = Settings {
        logs: Some(1),
        ..Settings::default()
    };
    gate.load(HashMap::from([(3, quota)]), time::now_ns());
    gate.used(3, Signal::Logs, Count::new(1, 1), time::now_ns());
    let body = r#"[{"message":"a"},{"message":"b"}]"#;
    let drain = |signature: &str| {
        post(
            "/vercel/logs",
            &[
                ("authorization", "Bearer key-c"),
                ("x-vercel-signature", signature),
            ],
            body,
        )
    };
    assert_eq!(s.send(drain(&"0".repeat(40))).await.status, 403);
    assert_eq!(refused(&s), []);
    let mut mac = Hmac::<sha1::Sha1>::new_from_slice(DRAIN_SECRET.as_bytes()).unwrap();
    mac.update(body.as_bytes());
    let signature = crate::text::hex(&mac.finalize().into_bytes());
    let r = s.send(drain(&signature)).await;
    assert_eq!(r.status, 200, "Vercel turns off a drain that keeps failing");
    assert_eq!(r.headers["x-sentry-rate-limits"], RATE);
    assert!(s.logs.try_recv().is_none());
    assert_eq!(refused(&s), [(3, "davy_quota", "log_item", 2)]);
}
