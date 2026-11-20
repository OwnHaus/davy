use super::deliver::{Transports, run, send};
use super::store::{self, Due};
use super::{Channel, Destination, Notice, Secret};
use crate::net::Reach;
use axum::{Router, http::HeaderMap, http::StatusCode, routing::post};
use std::time::Duration;
use tokio::sync::mpsc;

type Seen = mpsc::UnboundedReceiver<(HeaderMap, String)>;

/// A local receiver that answers `status` and hands over what it got.
async fn receiver(status: StatusCode) -> (String, Seen) {
    let (tx, rx) = mpsc::unbounded_channel();
    let app = Router::new().route(
        "/hook/secret-path",
        post(move |headers: HeaderMap, body: String| async move {
            tx.send((headers, body)).unwrap();
            (status, [("location", "http://127.0.0.1:1/")])
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/hook/secret-path", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, rx)
}

fn webhook(url: &str) -> Due {
    Due {
        id: 18,
        channel: 1,
        to: Channel::Webhook {
            url: url.to_owned(),
            secret: Secret::load("a1".repeat(32)),
        },
        payload: r#"{"id":18,"type":"test"}"#.to_owned(),
    }
}

fn header<'h>(headers: &'h HeaderMap, name: &str) -> Option<&'h str> {
    headers.get(name).map(|v| v.to_str().unwrap())
}

#[tokio::test]
async fn a_webhook_gets_the_body_with_its_delivery_id_and_signature() {
    let (url, mut seen) = receiver(StatusCode::NO_CONTENT).await;
    let sent = send(
        &Transports::new(Reach::Any, None).unwrap(),
        &webhook(&url),
        1_795_164_121_999,
    )
    .await;
    assert_eq!(sent, Ok(()));
    let (headers, body) = seen.recv().await.unwrap();
    assert_eq!(body, r#"{"id":18,"type":"test"}"#);
    assert_eq!(header(&headers, "content-type"), Some("application/json"));
    assert_eq!(header(&headers, "davy-delivery"), Some("18"));
    assert_eq!(
        header(&headers, "davy-signature"),
        Some("t=1795164121,v1=7f853164c56305474a83e2915c371a155f004fcf1e8566e8b3fa968f58271262")
    );
    assert!(header(&headers, "user-agent").unwrap().starts_with("Davy/"));
}

#[tokio::test]
async fn slack_gets_no_signature_and_an_error_status_or_redirect_fails_the_send() {
    let (url, mut seen) = receiver(StatusCode::INTERNAL_SERVER_ERROR).await;
    let slack = Due {
        to: Channel::Slack { url },
        ..webhook("")
    };
    let sent = send(&Transports::new(Reach::Any, None).unwrap(), &slack, 0).await;
    assert_eq!(sent, Err("HTTP 500".to_owned()));
    let (headers, _) = seen.recv().await.unwrap();
    assert_eq!(header(&headers, "davy-signature"), None);
    let (url, _seen) = receiver(StatusCode::FOUND).await;
    let sent = send(
        &Transports::new(Reach::Any, None).unwrap(),
        &webhook(&url),
        0,
    )
    .await;
    assert_eq!(sent, Err("HTTP 302".to_owned()));
}

#[tokio::test]
async fn a_refused_connection_names_the_cause_but_not_the_url() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/hook/secret-path", listener.local_addr().unwrap());
    drop(listener);
    let error = send(
        &Transports::new(Reach::Any, None).unwrap(),
        &webhook(&url),
        0,
    )
    .await
    .unwrap_err();
    assert!(error.contains("onnect"), "{error}");
    assert!(!error.contains("secret-path"), "{error}");
    assert!(!error.contains("127.0.0.1"), "{error}");
}

#[tokio::test]
async fn a_public_reach_sends_nothing_to_a_local_address() {
    let (url, mut seen) = receiver(StatusCode::OK).await;
    let public = Transports::new(Reach::Public, None).unwrap();
    let sent = send(&public, &webhook(&url), 0).await;
    assert_eq!(sent, Err("address is not public".to_owned()));
    let port = url.split(':').nth(2).unwrap().split('/').next().unwrap();
    let by_name = format!("http://localhost:{port}/hook/secret-path");
    let error = send(&public, &webhook(&by_name), 0).await.unwrap_err();
    assert!(error.ends_with("address is not public"), "{error}");
    assert!(!error.contains("secret-path"), "{error}");
    assert!(seen.try_recv().is_err());
}

#[tokio::test]
async fn the_notifier_sends_a_channel_s_rows_in_order_and_marks_them_sent() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("target/test-alert-{}", uuid::Uuid::new_v4()));
    let c = crate::store::open(&dir).unwrap();
    crate::store::create_project(&c, "shop").unwrap();
    let (url, mut seen) = receiver(StatusCode::OK).await;
    let url = crate::monitor::CheckUrl::parse(&url).unwrap();
    store::add(&c, Some(1), &Destination::Webhook(url), 0).unwrap();
    store::notify(&c, 1, &Notice::Test, 0).unwrap();
    store::notify(&c, 1, &Notice::Test, 0).unwrap();
    let writer = crate::store::Writer::start(crate::store::open(&dir).unwrap(), 8);
    let notifier = tokio::spawn(run(
        crate::store::open(&dir).unwrap(),
        Transports::new(Reach::Any, None).unwrap(),
        writer.sender.clone(),
        Duration::from_millis(10),
    ));
    let first = seen.recv().await.unwrap();
    let second = seen.recv().await.unwrap();
    assert_eq!(header(&first.0, "davy-delivery"), Some("1"));
    assert_eq!(header(&second.0, "davy-delivery"), Some("2"));
    let unsent = || -> i64 {
        c.query_row(
            "SELECT count(*) FROM notifications WHERE sent_at IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    for _ in 0..200 {
        if unsent() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(unsent(), 0);
    notifier.abort();
    assert!(notifier.await.unwrap_err().is_cancelled());
    drop(writer.sender);
    // Joining on the runtime's one thread would stop it from dropping the
    // aborted sends, which hold writer senders.
    tokio::task::spawn_blocking(move || writer.thread.join().unwrap())
        .await
        .unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}
