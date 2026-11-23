use super::deliver::{Transports, send};
use super::mail::Smtp;
use super::store::Due;
use super::{Channel, Kind, Notice};
use crate::net::Reach;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::sync::mpsc;

/// A local SMTP server that answers `rcpt` to `RCPT TO` and hands over
/// each line it got, the message included.
async fn server(rcpt: &'static str) -> (String, mpsc::UnboundedReceiver<String>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("smtp://{}", listener.local_addr().unwrap());
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let (read, mut write) = socket.into_split();
        let mut lines = BufReader::new(read).lines();
        write.write_all(b"220 test\r\n").await.unwrap();
        let mut data = false;
        while let Ok(Some(line)) = lines.next_line().await {
            let reply = if data {
                data = line != ".";
                (!data).then_some("250 queued\r\n")
            } else if line.starts_with("RCPT") {
                Some(rcpt)
            } else if line == "DATA" {
                data = true;
                Some("354 go on\r\n")
            } else if line == "QUIT" {
                Some("221 bye\r\n")
            } else {
                Some("250 ok\r\n")
            };
            tx.send(line).unwrap();
            if let Some(reply) = reply {
                write.write_all(reply.as_bytes()).await.unwrap();
            }
        }
    });
    (url, rx)
}

fn email(payload: String) -> Due {
    Due {
        id: 19,
        merged: Vec::new(),
        channel: 1,
        to: Channel::Email {
            to: "ops@example.com".into(),
        },
        payload,
    }
}

fn down() -> String {
    Notice::MonitorDown(super::Incident {
        id: 4,
        monitor: 1,
        name: "API".into(),
        reason: "HTTP 503".into(),
    })
    .render(Kind::Email, 19, Some("shop"), 1_795_164_190_000)
    .unwrap()
}

fn transports(url: &str) -> Transports {
    let smtp = Smtp::new(url, "Davy <alerts@davy.example>").unwrap();
    Transports::new(Reach::Any, Some(smtp)).unwrap()
}

#[tokio::test]
async fn an_email_row_goes_to_its_address_as_plain_text_with_a_stable_message_id() {
    let (url, mut seen) = server("250 ok\r\n").await;
    assert_eq!(send(&transports(&url), &email(down()), 0).await, Ok(()));
    let mut lines = Vec::new();
    while let Some(line) = seen.recv().await {
        lines.push(line);
    }
    let has = |want: &str| lines.iter().any(|l| l == want);
    assert!(has("MAIL FROM:<alerts@davy.example>"), "{lines:#?}");
    assert!(has("RCPT TO:<ops@example.com>"), "{lines:#?}");
    assert!(has("From: Davy <alerts@davy.example>"), "{lines:#?}");
    assert!(has("To: ops@example.com"), "{lines:#?}");
    assert!(has("Subject: [shop] API is down: HTTP 503"), "{lines:#?}");
    assert!(
        has("Message-ID: <davy.19.1795164190000@davy.example>"),
        "{lines:#?}"
    );
    assert!(has("Content-Type: text/plain; charset=utf-8"), "{lines:#?}");
    let body = lines.iter().skip_while(|l| !l.is_empty()).skip(1);
    assert_eq!(
        body.take_while(|l| *l != ".").cloned().collect::<Vec<_>>(),
        [
            "[shop] API is down: HTTP 503",
            "",
            "monitor: API",
            "monitor_id: 1",
            "incident_id: 4",
            "reason: HTTP 503",
            "at: 2026-11-20T08:43:10Z",
        ]
    );
}

#[tokio::test]
async fn a_refused_recipient_fails_the_send_with_the_server_s_answer() {
    let (url, _seen) = server("550 no such user\r\n").await;
    assert_eq!(
        send(&transports(&url), &email(down()), 0).await,
        Err("permanent error (550): no such user".to_owned())
    );
}

#[tokio::test(start_paused = true)]
async fn a_server_that_never_answers_times_out() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("smtp://{}", listener.local_addr().unwrap());
    let _held = tokio::spawn(async move { listener.accept().await });
    assert_eq!(
        send(&transports(&url), &email(down()), 0).await,
        Err("timeout".to_owned())
    );
}

#[tokio::test]
async fn without_an_smtp_server_an_email_row_fails() {
    let none = Transports::new(Reach::Any, None).unwrap();
    assert_eq!(
        send(&none, &email(down()), 0).await,
        Err("no SMTP server, set DAVY_SMTP_URL".to_owned())
    );
}

#[test]
fn a_password_goes_only_over_tls_and_no_error_shows_the_url() {
    let from = "alerts@davy.example";
    for ok in [
        "smtps://user:hunter2@mail.example",
        "smtp://user:hunter2@mail.example:587?tls=required",
        "smtp://relay.internal:25",
    ] {
        assert!(Smtp::new(ok, from).is_ok(), "{ok}");
    }
    let refused = |url: &str, from: &str| Smtp::new(url, from).unwrap_err().to_string();
    let only_tls = "DAVY_SMTP_URL sends a password only over TLS, use smtps:// or ?tls=required";
    let forms =
        "DAVY_SMTP_URL must be smtps://, smtp://...?tls=required or smtp:// without a password";
    for (url, want) in [
        ("smtp://user:hunter2@mail.example", only_tls),
        ("smtp://hunter2@mail.example", only_tls),
        ("smtp://user:hunter2@mail.example?tls=opportunistic", forms),
        ("smtps://user:hunter2@mail.example?tls=required", forms),
        ("https://user:hunter2@mail.example", forms),
        (
            "hunter2",
            "DAVY_SMTP_URL is not a URL: relative URL without a base",
        ),
    ] {
        assert_eq!(refused(url, from), want, "{url}");
    }
    assert!(
        refused("smtps://user:hunter2@mail.example", "alerts")
            .starts_with("--mail-from is not an address: ")
    );
}
