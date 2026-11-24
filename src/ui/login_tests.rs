//! How logins share the hash slots and wait after failed guesses.

use super::{PASSWORD, Ui, request, ui};
use crate::{time, ui::session};
use axum::body::Body;
use serde_json::json;
use std::sync::Arc;
use tower::ServiceExt;

#[tokio::test]
async fn a_login_waits_its_turn_for_a_hash_slot() {
    let s = ui();
    s.person("ann@example.com", time::now_ms());
    let held = busy_slots(&s);
    let body = json!({ "email": "ann@example.com", "password": PASSWORD }).to_string();
    let req = request("POST", "/ui/api/login", &[]).body(Body::from(body));
    let login = tokio::spawn(s.app.clone().oneshot(req.unwrap()));
    while s.local.queue.available_permits() == session::HASH_QUEUE {
        tokio::task::yield_now().await;
    }
    drop(held);
    assert_eq!(login.await.unwrap().unwrap().status(), 200);
}

#[tokio::test]
async fn login_is_429_at_once_when_the_queue_for_hash_slots_is_full() {
    let s = ui();
    s.person("ann@example.com", time::now_ms());
    let held = busy_slots(&s);
    let queue = u32::try_from(session::HASH_QUEUE).unwrap();
    let waiting = s.local.queue.try_acquire_many(queue).unwrap();
    let r = s.login("ann@example.com", PASSWORD).await;
    assert_eq!((r.status, r.header("retry-after")), (429, "1"));
    drop((held, waiting));
    let r = s.login("ann@example.com", PASSWORD).await;
    assert_eq!(r.status, 200, "a busy slot is no failed attempt");
}

#[tokio::test]
async fn a_login_in_its_wait_is_refused_before_it_takes_a_place_in_the_queue() {
    let s = ui();
    s.person("ann@example.com", time::now_ms());
    for email in ["ann@example.com", "nobody@example.com"] {
        assert_eq!(s.login(email, "not the password").await.status, 401);
        let queue = u32::try_from(session::HASH_QUEUE).unwrap();
        let _waiting = s.local.queue.try_acquire_many(queue).unwrap();
        let r = s.login(email, PASSWORD).await;
        let error = &r.body["error"];
        assert_eq!(
            (r.status, error.as_str()),
            (429, Some("too many attempts for this e-mail, wait"))
        );
    }
}

#[tokio::test(start_paused = true)]
async fn login_is_429_when_no_hash_slot_frees_in_time() {
    let s = ui();
    let _held = busy_slots(&s);
    let r = s.login("ann@example.com", PASSWORD).await;
    assert_eq!((r.status, r.header("retry-after")), (429, "1"));
}

/// Takes every hash slot until the result drops.
fn busy_slots(s: &Ui) -> Vec<tokio::sync::OwnedSemaphorePermit> {
    (0..session::HASHES_AT_ONCE)
        .map(|_| Arc::clone(&s.local.hashing).try_acquire_owned().unwrap())
        .collect()
}

#[tokio::test]
async fn slow_login_bodies_do_not_refuse_a_login() {
    let s = ui();
    s.person("ann@example.com", time::now_ms());
    let started = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let slow: Vec<_> = (0..1000)
        .map(|_| {
            let started = Arc::clone(&started);
            let first = futures::stream::once(async move {
                started.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Ok::<_, std::io::Error>("{")
            });
            let rest = futures::stream::pending();
            let body = Body::from_stream(futures::StreamExt::chain(first, rest));
            let req = request("POST", "/ui/api/login", &[]).body(body).unwrap();
            tokio::spawn(s.app.clone().oneshot(req))
        })
        .collect();
    while started.load(std::sync::atomic::Ordering::Relaxed) < slow.len() {
        tokio::task::yield_now().await;
    }
    tokio::task::yield_now().await;
    assert_eq!(s.login("ann@example.com", PASSWORD).await.status, 200);
    slow.iter().for_each(tokio::task::JoinHandle::abort);
}

#[tokio::test]
async fn failed_logins_for_one_email_wait_longer_each_time() {
    let s = ui();
    s.person("ann@example.com", time::now_ms());
    assert_eq!(
        s.login("ann@example.com", "not the password").await.status,
        401
    );
    let r = s.login("ann@example.com", PASSWORD).await;
    assert_eq!((r.status, r.header("retry-after")), (429, "1"));
    assert_eq!(s.login("bob@example.com", PASSWORD).await.status, 401);
}

#[tokio::test]
async fn failed_logins_do_not_keep_an_open_session_from_changing_the_password() {
    let s = ui();
    let mine = s.person("ann@example.com", time::now_ms());
    let r = s.login("ann@example.com", "not the password").await;
    assert_eq!(r.status, 401);
    assert_eq!(s.login("ann@example.com", PASSWORD).await.status, 429);
    let change = json!({ "current": PASSWORD, "new": "a new long password" });
    let r = s
        .post("/ui/api/account/password", &[("cookie", &mine)], &change)
        .await;
    assert_eq!(r.status, 204);
}

#[tokio::test]
async fn a_login_does_not_pay_from_the_ingest_budget() {
    let s = ui();
    s.person("ann@example.com", time::now_ms());
    let mut held = s.ingest.hold();
    held.take(32 << 20).unwrap();
    assert_eq!(s.login("ann@example.com", PASSWORD).await.status, 200);
}
