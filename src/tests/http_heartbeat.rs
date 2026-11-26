//! The heartbeat routes a cron job pings.

use super::*;

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
    assert_eq!(busy(&s), [(0, 1)], "a token picks no project");
}
