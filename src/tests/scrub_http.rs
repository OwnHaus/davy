use super::*;
use crate::{items::SdkItem, monitor::checkin::CheckInId};
use serde_json::{Value, json};

#[tokio::test]
async fn events_are_scrubbed_before_the_writer_queue() {
    let s = server();
    let request = s.send(post(
        "/api/1/store/?sentry_key=key-a",
        &[],
        json!({"message":"failure token=hidden", "extra":{"password":"hidden","author":"Ada","className":"Button","max_tokens":100,
                "authenticated":true,"token":42,"auth":false,"timestamp":1_732_441_234_561_u64,
                "microseconds":1_732_441_234_561_000_u64,"nanoseconds":1_732_441_234_561_000_000_u64,
                "card":4_242_424_242_424_242_u64},
            "frames":["Cache::get","std::fmt::write","Foo::Bar"],
            "contexts":{"os":{"name":"Linux"},"browser":{"name":"Chrome"},"runtime":{"name":"Rust"}},
            "sdk":{"name":"sentry.rust"},
            "request":{"headers":[["Cookie", "hidden"]]},
            "user":{"email":"a@example.com"}})
        .to_string(),
    ));
    let consume = async {
        loop {
            if let Ok(write) = s.error_inbox.try_recv() {
                let store::Job::Ingest { events, .. } = write.job else {
                    panic!("expected ingest")
                };
                assert_eq!(
                    events[0].message.as_deref(),
                    Some("failure token=[Filtered: sensitive]")
                );
                assert!(!events[0].raw.to_string().contains("hidden"));
                assert_eq!(events[0].raw["user"]["email"], "a@example.com");
                assert_eq!(
                    events[0].raw["extra"],
                    json!({"password":"[Filtered: sensitive]",
                    "author":"Ada","className":"Button","max_tokens":100,"authenticated":true,
                    "token":42,"auth":false,"timestamp":1_732_441_234_561_u64,
                    "microseconds":1_732_441_234_561_000_u64,"nanoseconds":1_732_441_234_561_000_000_u64,
                    "card":"[Filtered: card]"})
                );
                assert_eq!(
                    events[0].raw["frames"],
                    json!(["Cache::get", "std::fmt::write", "Foo::Bar"])
                );
                assert_eq!(
                    events[0].raw["contexts"],
                    json!({"os":{"name":"Linux"},
                    "browser":{"name":"Chrome"},"runtime":{"name":"Rust"}})
                );
                assert_eq!(events[0].raw["sdk"], json!({"name":"sentry.rust"}));
                write.done.send(Ok(())).unwrap();
                break;
            }
            tokio::task::yield_now().await;
        }
    };
    let (reply, ()) = tokio::join!(request, consume);
    assert_eq!(reply.status, 200);
}

#[tokio::test]
async fn logtail_is_scrubbed_before_the_index_queue() {
    let s = server();
    let reply = s
        .send(post(
            "/",
            &[
                ("authorization", "Bearer key-a"),
                ("content-type", "application/json"),
            ],
            r#"[{"message":"mail a@example.com from 192.0.2.1 assembly 4.0.0.0 Cache::get card 4242424242424242","attributes":{"api_key":"hidden","x-auth-token":"hidden","PHPSESSID":"hidden","access_token":"hidden"}}]"#,
        ))
        .await;
    assert_eq!(reply.status, 202);
    let logs = s.logs.try_recv().unwrap();
    assert_eq!(
        logs[0].message,
        "mail a@example.com from [Filtered: ip] assembly [Filtered: ip] Cache::get card [Filtered: card]"
    );
    assert!(
        !serde_json::to_string(&logs[0].attributes)
            .unwrap()
            .contains("hidden")
    );
}

/// Answers each queued job and returns what it was, until `n` arrived.
async fn jobs(s: &Server, n: usize) -> Vec<store::Job> {
    let mut jobs = Vec::new();
    while jobs.len() < n {
        if let Ok(write) = s.error_inbox.try_recv() {
            drop(write.done.send(Ok(())));
            jobs.push(write.job);
        }
        tokio::task::yield_now().await;
    }
    jobs
}

/// Sends an envelope for project `a` with one item of `kind`.
async fn envelope(s: &Server, kind: &str, payload: &Value, queued: usize) -> Vec<store::Job> {
    let body = format!("{{}}\n{{\"type\":\"{kind}\"}}\n{payload}\n");
    let (reply, jobs) = tokio::join!(
        s.send(post("/api/1/envelope/?sentry_key=key-a", &[], body)),
        jobs(s, queued)
    );
    assert_eq!(
        reply.status,
        200,
        "{}",
        String::from_utf8_lossy(&reply.body)
    );
    jobs
}

#[tokio::test]
async fn an_envelope_event_is_stored_scrubbed_with_its_release_kept() {
    let s = server();
    let event = json!({"message":"login from 192.0.2.1 failed, password=hunter2 sent",
        "release":"shop@1.2.3.4","environment":"prod","tags":{"server":"10.0.0.1"},
        "extra":{"by_ip":{"192.0.2.9":3},"cfg":"{\"api_key\":\"abc\"}"},
        "request":{"url":"https://x.test/?token=abc","data":"a=b",
            "headers":[{"name":"Authorization","value":"Bearer abc"}]}});
    let jobs = envelope(&s, "event", &event, 1).await;
    let [store::Job::Ingest { events, .. }] = &jobs[..] else {
        panic!("one ingest job");
    };
    let raw = &events[0].raw;
    assert_eq!(
        events[0].message.as_deref(),
        Some("login from [Filtered: ip] failed, password=[Filtered: sensitive]")
    );
    assert_eq!(
        (&raw["release"], &raw["environment"], &raw["tags"]),
        (
            &json!("shop@1.2.3.4"),
            &json!("prod"),
            &json!({"server":"[Filtered: ip]"})
        )
    );
    assert_eq!(
        raw["extra"],
        json!({"by_ip":{"[Filtered: ip]":3},"cfg":"{\"api_key=[Filtered: sensitive]}"})
    );
    assert_eq!(
        raw["request"],
        json!({"url":"https://x.test/?token=[Filtered: sensitive]",
            "data":"[Filtered: request body]",
            "headers":[{"name":"Authorization","value":"[Filtered: sensitive]"}]})
    );
}

#[tokio::test]
async fn otlp_logs_are_scrubbed_before_the_index_queue() {
    let s = server();
    let body = json!({"resourceLogs":[{"resource":{"attributes":[
        {"key":"service.name","value":{"stringValue":"api 192.0.2.3"}}]},
        "scopeLogs":[{"logRecords":[{"timeUnixNano":"1795165575662000000",
        "body":{"stringValue":"retry with Bearer abc.def"},"attributes":[
        {"key":"password","value":{"stringValue":"hunter2"}},
        {"key":"peer","value":{"stringValue":"192.0.2.4"}},
        {"key":"release","value":{"stringValue":"1.2.3.4"}}]}]}]}]});
    let reply = s.send(otlp("application/json", body.to_string())).await;
    assert_eq!(
        reply.status,
        200,
        "{}",
        String::from_utf8_lossy(&reply.body)
    );
    let logs = s.logs.try_recv().unwrap();
    assert_eq!(
        (logs[0].message.as_str(), logs[0].service.as_deref()),
        (
            "retry with [Filtered: credential]",
            Some("api [Filtered: ip]")
        )
    );
    assert_eq!(
        Value::Object(logs[0].attributes.clone()),
        json!({"password":"[Filtered: sensitive]","peer":"[Filtered: ip]","release":"1.2.3.4"})
    );
}

#[tokio::test]
async fn sentry_log_items_are_scrubbed_before_the_index_queue() {
    let s = server();
    let items = json!({"items":[{"timestamp":1_795_165_575.5,"level":"info",
        "body":"card 4242 4242 4242 4242 charged","attributes":{
        "session_id":{"value":"abc","type":"string"},
        "client.address":{"value":"192.0.2.5","type":"string"},
        "sentry.release":{"value":"shop@2.4.0","type":"string"}}}]});
    let reply = s
        .send(post(
            "/api/1/envelope/?sentry_key=key-a",
            &[],
            format!("{{}}\n{{\"type\":\"log\"}}\n{items}\n"),
        ))
        .await;
    assert_eq!(
        reply.status,
        200,
        "{}",
        String::from_utf8_lossy(&reply.body)
    );
    let logs = s.logs.try_recv().unwrap();
    assert_eq!(logs[0].message, "card [Filtered: card] charged");
    assert_eq!(logs[0].attributes["session_id"], "[Filtered: sensitive]");
    assert_eq!(logs[0].attributes["client.address"], "[Filtered: ip]");
    assert_eq!(logs[0].attributes["sentry.release"], "shop@2.4.0");
}

#[tokio::test]
async fn sessions_keep_the_ids_release_health_counts_users_by() {
    let s = server();
    let session = json!({"sid":"6e3a9a0bd0a14e4f8a6e1c2d3b4a5f60","did":"192.0.2.7",
        "init":true,"started":"2024-10-02T07:12:30Z","status":"ok","errors":0,
        "attrs":{"release":"web@1.2.3.4","environment":"prod","ip_address":"192.0.2.7"}});
    let jobs = envelope(&s, "session", &session, 1).await;
    let [store::Job::Items { items, .. }] = &jobs[..] else {
        panic!("one items job");
    };
    let [SdkItem::Session(one)] = &items[..] else {
        panic!("one session: {items:?}");
    };
    assert_eq!(
        (
            one.user.as_deref(),
            one.release.as_str(),
            one.environment.as_str()
        ),
        (Some("192.0.2.7"), "web@1.2.3.4", "prod")
    );
}

#[tokio::test]
async fn session_aggregates_keep_the_release_and_ids_release_health_counts_by() {
    let s = server();
    let aggregates = json!({"aggregates":[
        {"started":"2024-10-02T07:14:00.000Z","did":"192.0.2.7","exited":1}],
        "attrs":{"release":"api@1.2.3.4","environment":"10.0.0.1"}});
    let jobs = envelope(&s, "sessions", &aggregates, 1).await;
    let [store::Job::Items { items, .. }] = &jobs[..] else {
        panic!("one items job");
    };
    let [SdkItem::Sessions(many)] = &items[..] else {
        panic!("one aggregates item: {items:?}");
    };
    assert_eq!(
        (
            many.release.as_str(),
            many.environment.as_str(),
            many.counts[0].user.as_deref()
        ),
        ("api@1.2.3.4", "10.0.0.1", Some("192.0.2.7"))
    );
}

#[tokio::test]
async fn structural_names_are_scrubbed_below_an_events_root() {
    let s = server();
    let event = json!({"release":"shop@1.2.3.4","logger":"token=abc",
        "extra":{"platform":"password=hunter2; AKIAIOSFODNN7EXAMPLE",
        "logger":"4242424242424242","release":"Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2ln"},
        "tags":[["dist","4242424242424242"]],
        "contexts":{"trace":{"trace_id":"4bf92f3577b34da6a3ce929d0e0e4736"}}});
    let jobs = envelope(&s, "event", &event, 1).await;
    let [store::Job::Ingest { events, .. }] = &jobs[..] else {
        panic!("one ingest job");
    };
    let raw = &events[0].raw;
    assert_eq!(
        (&raw["release"], &raw["logger"], &raw["tags"]),
        (
            &json!("shop@1.2.3.4"),
            &json!("token=[Filtered: sensitive]"),
            &json!([["dist", "[Filtered: card]"]])
        )
    );
    assert_eq!(
        raw["extra"],
        json!({"platform":"password=[Filtered: sensitive]; [Filtered: credential]",
        "logger":"[Filtered: card]","release":"[Filtered: credential]"})
    );
    assert_eq!(
        events[0].trace_id.as_deref(),
        Some("4bf92f3577b34da6a3ce929d0e0e4736")
    );
}

#[tokio::test]
async fn a_check_in_keeps_its_slug_and_id() {
    let s = server();
    let id = "42424242-4242-4242-4242-424242424242";
    let check_in = json!({"check_in_id":id,"monitor_slug":"backup","status":"ok",
        "environment":"10.0.0.1","duration":1.5});
    let jobs = envelope(&s, "check_in", &check_in, 1).await;
    let [
        store::Job::CheckIn {
            item: Ok(check_in), ..
        },
    ] = &jobs[..]
    else {
        panic!("one check-in");
    };
    assert_eq!(
        (
            check_in.slug.as_str(),
            check_in.id.as_ref().map(CheckInId::as_str)
        ),
        ("backup", Some("42424242424242424242424242424242"))
    );
}

#[tokio::test]
async fn feedback_is_stored_scrubbed() {
    let s = server();
    let feedback = json!({"contexts":{"feedback":{"name":"Ana","contact_email":"ana@example.com",
        "message":"reset from 192.0.2.8 fails, link token=abc sent",
        "url":"https://x.test/reset?token=abc"}}});
    let jobs = envelope(&s, "feedback", &feedback, 1).await;
    let [store::Job::Items { items, .. }] = &jobs[..] else {
        panic!("one items job");
    };
    let [SdkItem::UserFeedback(f)] = &items[..] else {
        panic!("one feedback: {items:?}");
    };
    assert_eq!(
        (f.message.as_str(), f.url.as_deref()),
        (
            "reset from [Filtered: ip] fails, link token=[Filtered: sensitive]",
            Some("https://x.test/reset?token=[Filtered: sensitive]")
        )
    );
    assert_eq!(
        (f.name.as_deref(), f.email.as_deref()),
        (Some("Ana"), Some("ana@example.com")),
        "names and addresses are kept unless the policy asks"
    );
}

/// Sends an event whose message is `message`, answering its job, while
/// `held` bytes of the budget are taken.
async fn store_while_held(s: &Server, message: &str, held: usize) -> u16 {
    let mut taken = s.budget.hold();
    taken.take(held).unwrap();
    let body = json!({ "message": message }).to_string();
    let request = s.send(post("/api/1/store/?sentry_key=key-a", &[], body));
    let reply = tokio::select! {
        reply = request => reply,
        _ = async { loop { drop(jobs(s, 1).await); } } => unreachable!(),
    };
    drop(taken);
    reply.status
}

#[tokio::test]
async fn what_a_scrub_adds_is_paid_from_the_budget_and_given_back() {
    let s = server();
    let (plain, growing) = ("abcde ".repeat(20_000), "ghp_x ".repeat(20_000));
    // The most that can be held while the plain event is still taken.
    let (mut low, mut high) = (0, BUDGET);
    while high - low > 16 << 10 {
        let mid = low.midpoint(high);
        if store_while_held(&s, &plain, mid).await == 200 {
            low = mid;
        } else {
            high = mid;
        }
    }
    assert_eq!(store_while_held(&s, &plain, low).await, 200);
    assert_eq!(
        store_while_held(&s, &growing, low).await,
        429,
        "each credential grows by 17 bytes, 340 KB in all"
    );
    assert_eq!(store_while_held(&s, &growing, 0).await, 200);
    s.budget.hold().take(BUDGET).unwrap();
}
