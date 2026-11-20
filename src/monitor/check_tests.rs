use super::check::{Checked, Http, check, client, run};
use super::{CheckUrl, Expect, NewMonitor, Outcome, Reason, State, Target, store};
use crate::net::{self, Reach};
use axum::{Router, http::StatusCode, response::Redirect, routing::get};
use std::time::Duration;

/// A local site with one route per case.
async fn site() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let to = |url: String| {
        get(move || {
            let url = url.clone();
            async move { Redirect::to(&url) }
        })
    };
    let app = Router::new()
        .route("/ok", get(|| async { "all good" }))
        .route(
            "/down",
            get(|| async { (StatusCode::SERVICE_UNAVAILABLE, "") }),
        )
        .route(
            "/slow",
            get(|| async {
                tokio::time::sleep(Duration::from_secs(2)).await;
                "late"
            }),
        )
        .route(
            "/big",
            get(|| async { format!("{}needle", "a".repeat(1024 * 1024)) }),
        )
        .route("/hop", get(|| async { Redirect::to("/ok") }))
        .route("/loop", get(|| async { Redirect::to("/loop") }))
        .route("/to-literal", to(format!("http://127.0.0.1:{port}/ok")))
        .route("/to-name", to(format!("http://localhost:{port}/ok")));
    let base = format!("http://127.0.0.1:{port}");
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}

fn http(url: &str, expect: Expect) -> Http {
    Http {
        id: 1,
        url: CheckUrl::parse(url).unwrap(),
        expect,
        timeout: Duration::from_millis(500),
    }
}

/// The outcome and status of one check, without its time.
async fn outcome(url: &str, expect: Expect) -> (Outcome, Option<u16>) {
    outcome_with(&client(Reach::Any).unwrap(), url, expect).await
}

async fn outcome_with(client: &net::Client, url: &str, expect: Expect) -> (Outcome, Option<u16>) {
    let Checked {
        outcome, status, ..
    } = check(client, &http(url, expect)).await;
    (outcome, status)
}

fn keyword(k: &str) -> Expect {
    Expect::Keyword(k.to_owned())
}

fn no_keyword(k: &str) -> Expect {
    Expect::NoKeyword(k.to_owned())
}

#[tokio::test]
async fn a_status_below_400_is_up_and_any_other_is_down() {
    let site = site().await;
    let up = outcome(&format!("{site}/ok"), Expect::Status).await;
    assert_eq!(up, (Outcome::Up, Some(200)));
    let down = outcome(&format!("{site}/down"), Expect::Status).await;
    assert_eq!(down, (Outcome::Down(Reason::Status(503)), Some(503)));
}

#[tokio::test]
async fn a_keyword_must_be_in_the_body_and_a_no_keyword_must_not() {
    let ok = format!("{}/ok", site().await);
    let cases = [
        (keyword("good"), Outcome::Up),
        (keyword("bad"), Outcome::Down(Reason::KeywordMissing)),
        (keyword("Good"), Outcome::Down(Reason::KeywordMissing)),
        (no_keyword("good"), Outcome::Down(Reason::KeywordFound)),
        (no_keyword("bad"), Outcome::Up),
    ];
    for (expect, want) in cases {
        assert_eq!(outcome(&ok, expect).await, (want, Some(200)));
    }
}

#[tokio::test]
async fn only_the_first_mebibyte_of_the_body_is_searched() {
    let big = format!("{}/big", site().await);
    let missing = outcome(&big, keyword("needle")).await;
    assert_eq!(missing, (Outcome::Down(Reason::KeywordMissing), Some(200)));
    assert_eq!(
        outcome(&big, keyword("aaa")).await,
        (Outcome::Up, Some(200))
    );
}

#[tokio::test]
async fn a_redirect_is_followed_up_to_five_times() {
    let site = site().await;
    let hop = outcome(&format!("{site}/hop"), keyword("good")).await;
    assert_eq!(hop, (Outcome::Up, Some(200)));
    let (looped, status) = outcome(&format!("{site}/loop"), Expect::Status).await;
    assert_eq!(status, None);
    let Outcome::Down(Reason::Connect(e)) = looped else {
        panic!("a redirect loop is a connection failure, got {looped:?}");
    };
    assert!(e.contains("redirect"), "{e}");
}

#[tokio::test]
async fn a_public_reach_refuses_a_local_address_by_literal_by_name_and_on_a_redirect() {
    let site = site().await;
    let port: u16 = site.rsplit(':').next().unwrap().parse().unwrap();
    let local = ([127, 0, 0, 1], port).into();
    let public = net::Client::resolving(Reach::Public, 5, "site.test", local).unwrap();
    let stand_in = format!("http://site.test:{port}");
    let judged = [
        format!("{stand_in}/ok"),
        format!("{stand_in}/to-literal"),
        format!("{stand_in}/to-name"),
        format!("{site}/ok"),
        format!("http://localhost:{port}/ok"),
        format!("http://[::ffff:127.0.0.1]:{port}/ok"),
        format!("http://0x7f000001:{port}/ok"),
    ];
    let mut got = Vec::new();
    for url in &judged {
        got.push(outcome_with(&public, url, Expect::Status).await);
    }
    let refused = (Outcome::Down(Reason::NotPublic), None);
    assert_eq!(
        got,
        [
            (Outcome::Up, Some(200)),
            refused.clone(),
            refused.clone(),
            refused.clone(),
            refused.clone(),
            refused.clone(),
            refused,
        ]
    );
    let any = client(Reach::Any).unwrap();
    for hop in ["to-literal", "to-name"] {
        let reached = outcome_with(&any, &format!("{site}/{hop}"), Expect::Status).await;
        assert_eq!(reached, (Outcome::Up, Some(200)), "{hop}");
    }
}

#[tokio::test]
async fn a_slow_response_is_a_timeout() {
    let slow = format!("{}/slow", site().await);
    let checked = check(&client(Reach::Any).unwrap(), &http(&slow, Expect::Status)).await;
    assert_eq!(checked.outcome, Outcome::Down(Reason::Timeout));
    assert_eq!(checked.status, None);
    assert!((500..1500).contains(&checked.ms), "{}", checked.ms);
}

#[tokio::test]
async fn a_refused_connection_names_the_cause_but_not_the_address() {
    let (refused, status) = outcome("http://127.0.0.1:1/health", Expect::Status).await;
    assert_eq!(status, None);
    let Outcome::Down(Reason::Connect(e)) = refused else {
        panic!("expected a connection failure, got {refused:?}");
    };
    assert!(e.contains("onnect"), "{e}");
    assert!(!e.contains("127.0.0.1") && !e.contains("health"), "{e}");
}

#[tokio::test]
async fn the_scheduler_checks_a_new_monitor_and_stores_the_result() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("target/test-check-{}", uuid::Uuid::new_v4()));
    let c = crate::store::open(&dir).unwrap();
    crate::store::create_project(&c, "shop").unwrap();
    let target = Target::Http {
        url: CheckUrl::parse(&format!("{}/down", site().await)).unwrap(),
        expect: Expect::Status,
        timeout: Duration::from_secs(5),
    };
    let m = NewMonitor::new("API", target, None).unwrap();
    let id = store::add(&c, 1, &m, 0).unwrap().id;
    let writer = crate::store::Writer::start(crate::store::open(&dir).unwrap(), 8);
    let scheduler = tokio::spawn(run(
        crate::store::open(&dir).unwrap(),
        client(Reach::Any).unwrap(),
        writer.sender.clone(),
        Duration::from_millis(10),
    ));
    let checks = || -> Vec<(bool, Option<i64>, Option<String>)> {
        c.prepare("SELECT ok,status,reason FROM checks WHERE monitor_id=?1")
            .unwrap()
            .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    for _ in 0..200 {
        if !checks().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // The second check waits 10 s for confirmation, so 100 ms adds none.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(checks(), [(false, Some(503), Some("HTTP 503".to_owned()))]);
    let state = store::list(&c).unwrap()[0].state;
    assert_eq!(state, State::Failing);
    scheduler.abort();
    assert!(scheduler.await.unwrap_err().is_cancelled());
    drop(writer.sender);
    tokio::task::spawn_blocking(move || writer.thread.join().unwrap())
        .await
        .unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}
