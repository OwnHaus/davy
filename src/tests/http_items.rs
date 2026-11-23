//! Client reports and user feedback in a Sentry envelope.

use super::*;
use crate::items::{SdkItem, client_report::Discarded};

const EVENT: &str = "0123456789abcdef0123456789abcdef";

/// Answers each queued job and returns what it was, until `n` arrived.
async fn take(s: &Server, n: usize) -> Vec<store::Job> {
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

fn feedback(message: &str) -> String {
    format!(
        "{{\"type\":\"feedback\"}}\n{{\"type\":\"feedback\",\"contexts\":{{\"feedback\":{{\"message\":\"{message}\",\"associated_event_id\":\"{EVENT}\"}}}}}}\n"
    )
}

#[tokio::test]
async fn a_client_report_and_feedback_are_queued_for_their_project() {
    let s = server();
    let body = format!(
        "{{}}\n{{\"type\":\"client_report\"}}\n{{\"discarded_events\":[{{\"reason\":\"before_send\",\"category\":\"error\",\"quantity\":3}}]}}\n{}",
        feedback("Pay does nothing")
    );
    let (r, jobs) = tokio::join!(
        s.send(post("/api/2/envelope/?sentry_key=key-b", &[], body)),
        take(&s, 1)
    );
    assert_eq!(r.status, 200);
    let [store::Job::Items { project, items, .. }] = &jobs[..] else {
        panic!("one items job");
    };
    assert_eq!(*project, 2);
    let SdkItem::UserFeedback(f) = &items[1] else {
        panic!("feedback second");
    };
    assert_eq!(
        (
            f.message.as_str(),
            f.event_id.as_ref().map(|e| e.0.as_str())
        ),
        ("Pay does nothing", Some(EVENT))
    );
    assert_eq!(
        items[0],
        SdkItem::ClientReport(vec![Discarded {
            reason: "before_send".to_owned(),
            category: "error".to_owned(),
            quantity: 3,
        }])
    );
}

#[tokio::test]
async fn the_event_is_queued_before_the_items_of_its_envelope() {
    let s = server();
    let body = format!(
        "{{}}\n{}{{\"type\":\"event\"}}\n{{\"message\":\"boom\"}}\n",
        feedback("hi")
    );
    let (r, jobs) = tokio::join!(
        s.send(post("/api/1/envelope/?sentry_key=key-a", &[], body)),
        take(&s, 2)
    );
    assert_eq!(r.status, 200);
    assert!(matches!(jobs[0], store::Job::Ingest { .. }), "event first");
    assert!(matches!(jobs[1], store::Job::Items { .. }), "items second");
}

#[tokio::test]
async fn an_envelope_gives_at_most_ten_items_and_skips_bad_ones() {
    let s = server();
    let body = format!(
        "{{}}\n{{\"type\":\"user_report\"}}\nnot json\n{{\"type\":\"feedback\"}}\n{{\"contexts\":{{}}}}\n{}",
        feedback("x").repeat(12)
    );
    let (r, jobs) = tokio::join!(
        s.send(post("/api/1/envelope/?sentry_key=key-a", &[], body)),
        take(&s, 1)
    );
    assert_eq!(r.status, 200);
    let [store::Job::Items { items, .. }] = &jobs[..] else {
        panic!("one items job");
    };
    assert_eq!(items.len(), 10);
}

#[tokio::test]
async fn an_envelope_with_nothing_to_keep_queues_nothing() {
    let s = server();
    let body = "{}\n{\"type\":\"client_report\"}\n[1]\n{\"type\":\"session\"}\n{}\n";
    let r = s
        .send(post("/api/1/envelope/?sentry_key=key-a", &[], body))
        .await;
    assert_eq!(r.status, 200);
    assert!(s.error_inbox.try_recv().is_err(), "nothing queued");
}
