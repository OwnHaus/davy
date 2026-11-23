//! The contract of the UI routes, sent through the whole server router.

use super::{PROTECTED, account::next_page, assets, session::Local};
use crate::{
    accounts::{INVITE_MS, SESSION_MAX_MS, Secret, hash_password, store as accounts},
    http::{App, Keys, body::Budget},
    logs::store::LogQueue,
    store, time,
};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{HeaderMap, Request, header},
};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;

const ORIGIN: &str = "https://davy.example";
const PASSWORD: &str = "correct horse battery";

struct Ui {
    app: Router,
    local: Arc<Local>,
    reads: Arc<super::issues::Reads>,
    db: Connection,
    /// The memory budget of ingest bodies.
    ingest: Budget,
    writer: store::Writer,
}

struct Reply {
    status: u16,
    headers: HeaderMap,
    body: Value,
}

impl Reply {
    fn header(&self, name: &str) -> &str {
        self.headers.get(name).map_or("", |v| v.to_str().unwrap())
    }

    /// The `name=value` part of the session cookie it sets.
    fn cookie(&self) -> String {
        let set = self.header("set-cookie");
        set.split(';').next().unwrap().to_owned()
    }
}

/// The server router with the UI at `https://davy.example/` and no people.
fn ui() -> Ui {
    let data = crate::test_support::scratch("ui");
    let db = store::open(&data).unwrap();
    let writer = store::Writer::start(store::open(&data).unwrap(), 1);
    let ingest = Budget::new(32 << 20);
    let local = Arc::new(Local::new(store::open(&data).unwrap(), writer.ui.clone()));
    let app = App {
        budget: ingest.clone(),
        keys: Arc::new(Keys::new(store::open(&data).unwrap())),
        sender: writer.sender.clone(),
        logs: LogQueue::bounded(1, 1 << 20).0,
        sourcemaps: Arc::new(crate::sourcemaps::Store::open(std::path::Path::new(
            "target/no-sourcemaps",
        ))),
    };
    let public_url = url::Url::parse("https://davy.example/").unwrap();
    let reads = super::issues::Reads::new(
        store::open(&data).unwrap(),
        data.clone(),
        ingest.clone(),
        writer.ui.clone(),
    );
    let reads = Arc::new(reads);
    let routes = super::router(Arc::clone(&local), Arc::clone(&reads), &public_url);
    Ui {
        app: crate::http::router(app, routes),
        local,
        reads,
        db,
        ingest,
        writer,
    }
}

/// A request from the browser at the public URL. A write carries JSON and
/// the public origin.
fn request(method: &str, uri: &str, headers: &[(&str, &str)]) -> axum::http::request::Builder {
    let mut req = Request::builder().method(method).uri(uri);
    if !headers.iter().any(|(name, _)| *name == "host") {
        req = req.header(header::HOST, "davy.example");
    }
    if method != "GET" {
        req = req
            .header(header::ORIGIN, ORIGIN)
            .header(header::CONTENT_TYPE, "application/json");
    }
    for (name, value) in headers {
        req = req.header(*name, *value);
    }
    req
}

impl Ui {
    async fn send(&self, req: Request<Body>) -> Reply {
        let r = self.app.clone().oneshot(req).await.unwrap();
        let status = r.status().as_u16();
        let headers = r.headers().clone();
        let bytes = to_bytes(r.into_body(), 1 << 20).await.unwrap();
        let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        Reply {
            status,
            headers,
            body,
        }
    }

    async fn get(&self, uri: &str, headers: &[(&str, &str)]) -> Reply {
        self.send(request("GET", uri, headers).body(Body::empty()).unwrap())
            .await
    }

    async fn post(&self, uri: &str, headers: &[(&str, &str)], body: &Value) -> Reply {
        let req = request("POST", uri, headers).body(Body::from(body.to_string()));
        self.send(req.unwrap()).await
    }

    async fn login(&self, email: &str, password: &str) -> Reply {
        let body = json!({ "email": email, "password": password });
        self.post("/ui/api/login", &[], &body).await
    }

    /// Adds `email` at `at` and returns its welcome link's secret.
    fn invite(&self, email: &str, at: i64) -> Secret {
        let secret = Secret::random().unwrap();
        let email = crate::accounts::Email::parse(email).unwrap();
        accounts::add(&self.db, &email, &secret.hash(), at)
            .unwrap()
            .unwrap();
        secret
    }

    /// A person `email` with the password [`PASSWORD`] whose session started
    /// at `at`, as a `Cookie` value.
    fn person(&self, email: &str, at: i64) -> String {
        let invite = self.invite(email, at);
        let session = Secret::random().unwrap();
        let welcome = accounts::Change::Welcome {
            invite: invite.hash(),
            password: hash_password(PASSWORD).unwrap(),
            session: session.hash(),
            at,
        };
        assert_eq!(accounts::apply(&self.db, &welcome).unwrap(), Ok(()));
        format!("{}={}", super::session::COOKIE, session.encode())
    }
}

#[tokio::test]
async fn every_protected_route_needs_a_live_session() {
    let s = ui();
    let now = time::now_ms();
    let live = s.person("ann@example.com", now);
    let expired = s.person("bob@example.com", now - SESSION_MAX_MS - 1000);
    let removed = s.person("cid@example.com", now);
    accounts::remove(
        &s.db,
        &crate::accounts::Email::parse("cid@example.com").unwrap(),
    )
    .unwrap();
    let me = s.get("/ui/api/me", &[("cookie", &live)]).await;
    assert_eq!(
        (me.status, me.body),
        (200, json!({ "email": "ann@example.com" }))
    );
    let bearer = "Bearer a-test-token-that-is-long-enough!";
    let refused: [&[(&str, &str)]; 5] = [
        &[],
        &[("cookie", &expired)],
        &[("cookie", &removed)],
        &[("cookie", "__Host-davy_session=not-a-secret")],
        &[("authorization", bearer)],
    ];
    for (method, uri) in PROTECTED {
        for headers in refused {
            let req = request(method, uri, headers)
                .body(Body::from("{}"))
                .unwrap();
            let r = s.send(req).await;
            assert_eq!(
                (r.status, &r.body),
                (401, &json!({ "error": "log in first" })),
                "{method} {uri} {headers:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_write_needs_json_and_the_exact_public_origin() {
    let s = ui();
    let body = json!({ "email": "ann@example.com", "password": PASSWORD }).to_string();
    let send = |origin: Option<&str>, content_type: &str| {
        let mut req = Request::post("/ui/api/login")
            .header(header::HOST, "davy.example")
            .header(header::CONTENT_TYPE, content_type);
        if let Some(origin) = origin {
            req = req.header(header::ORIGIN, origin);
        }
        req.body(Body::from(body.clone())).unwrap()
    };
    for (origin, content_type) in [
        (None, "application/json"),
        (Some("https://davy.example:8443"), "application/json"),
        (Some("http://davy.example"), "application/json"),
        (Some("https://evil.example"), "application/json"),
        (Some("null"), "application/json"),
        (Some(ORIGIN), "text/plain"),
        (Some(ORIGIN), "application/x-www-form-urlencoded"),
    ] {
        let r = s.send(send(origin, content_type)).await;
        assert_eq!(r.status, 403, "{origin:?} {content_type}");
    }
    let r = s
        .send(send(Some(ORIGIN), "application/json; charset=utf-8"))
        .await;
    assert_eq!(r.status, 401);
}

#[tokio::test]
async fn a_host_that_names_another_server_is_403() {
    let s = ui();
    for (host, status) in [
        ("davy.example", 200),
        ("localhost:3000", 200),
        ("127.0.0.1:8080", 200),
        ("[::1]:8080", 200),
        ("DAVY.EXAMPLE", 200),
        ("evil.example", 403),
        ("davy.example.evil.example", 403),
    ] {
        let r = s.get("/ui/", &[("host", host)]).await;
        assert_eq!(r.status, status, "{host}");
    }
    let two = Request::get("/ui/")
        .header("host", "davy.example")
        .header("host", "evil.example");
    assert_eq!(s.send(two.body(Body::empty()).unwrap()).await.status, 403);
}

#[tokio::test]
async fn login_answers_a_wrong_password_like_an_unknown_email() {
    let s = ui();
    s.person("ann@example.com", time::now_ms());
    let wrong = s.login("ann@example.com", "not the password").await;
    let unknown = s.login("nobody@example.com", PASSWORD).await;
    assert_eq!((wrong.status, &wrong.body), (unknown.status, &unknown.body));
    assert_eq!(wrong.body, json!({ "error": "wrong e-mail or password" }));
    assert_eq!(wrong.header("set-cookie"), "");
}

#[tokio::test]
async fn login_sets_a_host_only_cookie_and_keeps_next_inside_the_ui() {
    let s = ui();
    s.person("ann@example.com", time::now_ms());
    let body =
        json!({ "email": " Ann@Example.com", "password": PASSWORD, "next": "/ui/issues/5?x=1" });
    let r = s.post("/ui/api/login", &[], &body).await;
    assert_eq!(
        (r.status, &r.body),
        (
            200,
            &json!({ "email": "ann@example.com", "next": "/ui/issues/5?x=1" })
        )
    );
    let set = r.header("set-cookie");
    assert!(set.starts_with("__Host-davy_session="), "{set}");
    assert!(
        set.ends_with("; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age=7776000"),
        "{set}"
    );
    let me = s.get("/ui/api/me", &[("cookie", &r.cookie())]).await;
    assert_eq!(me.status, 200);
}

#[test]
fn next_is_ignored_unless_it_is_a_path_under_ui() {
    for (next, page) in [
        (None, "/ui/"),
        (Some("/ui/issues/5?x=1#top"), "/ui/issues/5?x=1#top"),
        (Some("/ui/"), "/ui/"),
        (Some("//evil.example/ui/"), "/ui/"),
        (Some("/\\evil.example/ui/"), "/ui/"),
        (Some("https://evil.example/ui/"), "/ui/"),
        (Some("/ui/../admin"), "/ui/"),
        (Some("/ui/%2e%2e/admin"), "/ui/"),
        (Some("javascript:alert(1)"), "/ui/"),
        (Some("/uix"), "/ui/"),
        (Some(""), "/ui/"),
    ] {
        assert_eq!(next_page(next), page, "{next:?}");
    }
}

#[tokio::test]
async fn login_is_429_at_once_when_both_hash_slots_are_busy() {
    let s = ui();
    s.person("ann@example.com", time::now_ms());
    let held: Vec<_> = (0..super::session::HASHES_AT_ONCE)
        .map(|_| Arc::clone(&s.local.hashing).try_acquire_owned().unwrap())
        .collect();
    let r = s.login("ann@example.com", PASSWORD).await;
    assert_eq!((r.status, r.header("retry-after")), (429, "1"));
    drop(held);
    let r = s.login("ann@example.com", PASSWORD).await;
    assert_eq!(r.status, 200, "a busy slot is no failed attempt");
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
async fn a_welcome_link_works_once_and_for_a_day() {
    let s = ui();
    let now = time::now_ms();
    let secret = s.invite("ann@example.com", now);
    let welcome = |secret: &str, password: &str| json!({ "secret": secret, "password": password });
    let short = s
        .post("/ui/api/welcome", &[], &welcome(&secret.encode(), "short"))
        .await;
    assert_eq!(short.status, 400);
    let r = s
        .post("/ui/api/welcome", &[], &welcome(&secret.encode(), PASSWORD))
        .await;
    assert_eq!(
        (r.status, &r.body),
        (200, &json!({ "email": "ann@example.com" }))
    );
    assert_eq!(
        s.get("/ui/api/me", &[("cookie", &r.cookie())]).await.status,
        200
    );
    let again = s
        .post("/ui/api/welcome", &[], &welcome(&secret.encode(), PASSWORD))
        .await;
    assert_eq!(again.status, 410);
    let old = s.invite("bob@example.com", now - INVITE_MS - 1000);
    let late = s
        .post("/ui/api/welcome", &[], &welcome(&old.encode(), PASSWORD))
        .await;
    assert_eq!(late.status, 410);
    let bad = s
        .post("/ui/api/welcome", &[], &welcome("not-a-secret", PASSWORD))
        .await;
    assert_eq!(bad.status, 410);
    assert_eq!(s.login("ann@example.com", PASSWORD).await.status, 200);
}

#[tokio::test]
async fn a_new_password_ends_the_other_sessions() {
    let s = ui();
    let mine = s.person("ann@example.com", time::now_ms());
    let other = s.login("ann@example.com", PASSWORD).await.cookie();
    let cookie = [("cookie", mine.as_str())];
    let right = json!({ "current": PASSWORD, "new": "a new long password" });
    assert_eq!(
        s.post("/ui/api/account/password", &cookie, &right)
            .await
            .status,
        204
    );
    assert_eq!(s.get("/ui/api/me", &cookie).await.status, 200);
    assert_eq!(s.get("/ui/api/me", &[("cookie", &other)]).await.status, 401);
    let again = s.post("/ui/api/account/password", &cookie, &right).await;
    assert_eq!(
        (again.status, &again.body),
        (400, &json!({ "error": "the current password is wrong" }))
    );
    let r = s.post("/ui/api/account/password", &cookie, &right).await;
    assert_eq!(
        (r.status, r.header("retry-after")),
        (429, "1"),
        "the wrong current password counts as a failed attempt"
    );
    let r = s.login("ann@example.com", "a new long password").await;
    assert_eq!(r.status, 200, "a failed change does not count for login");
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

/// The UI of `data` at `public_url` whose writer is gone, so any write is
/// a `500`.
fn without_writer(data: &std::path::Path, public_url: &str) -> Router {
    let (writes, gone) = crossbeam_channel::bounded(1);
    drop(gone);
    let local = Arc::new(Local::new(store::open(data).unwrap(), writes.clone()));
    let reads = super::issues::Reads::new(
        store::open(data).unwrap(),
        data.to_owned(),
        Budget::new(1 << 20),
        writes,
    );
    super::router(
        local,
        Arc::new(reads),
        &url::Url::parse(public_url).unwrap(),
    )
}

#[tokio::test]
async fn logout_writes_only_for_a_session_that_exists() {
    let s = ui();
    let data = crate::test_support::scratch("ui-no-writer");
    let ui = Ui {
        app: without_writer(&data, "https://davy.example/"),
        ..s
    };
    let session = Secret::random().unwrap();
    let cookie = format!("{}={}", super::session::COOKIE, session.encode());
    let r = ui
        .post("/ui/api/logout", &[("cookie", &cookie)], &json!({}))
        .await;
    assert_eq!(r.status, 204, "no write was tried");
    let c = store::open(&data).unwrap();
    let now = time::now_ms();
    let invite = Secret::random().unwrap();
    let email = crate::accounts::Email::parse("ann@example.com").unwrap();
    accounts::add(&c, &email, &invite.hash(), now)
        .unwrap()
        .unwrap();
    let welcome = accounts::Change::Welcome {
        invite: invite.hash(),
        password: hash_password(PASSWORD).unwrap(),
        session: session.hash(),
        at: now,
    };
    assert_eq!(accounts::apply(&c, &welcome).unwrap(), Ok(()));
    let r = ui
        .post("/ui/api/logout", &[("cookie", &cookie)], &json!({}))
        .await;
    assert_eq!(
        (r.status, &r.body),
        (
            500,
            &json!({ "error": "internal error, the server log says more" })
        ),
        "the error text stays in the log"
    );
}

#[tokio::test]
async fn an_ipv6_public_url_is_an_allowed_host() {
    let s = ui();
    let data = crate::test_support::scratch("ui-ipv6");
    let ui = Ui {
        app: without_writer(&data, "https://[2001:db8::1]/"),
        ..s
    };
    let r = ui.get("/ui/api/me", &[("host", "[2001:db8::1]")]).await;
    assert_eq!(r.status, 401);
    let r = ui.get("/ui/api/me", &[("host", "[2001:db8::2]")]).await;
    assert_eq!(r.status, 403);
}

#[tokio::test]
async fn logout_ends_this_session_and_logout_others_the_rest() {
    let s = ui();
    let mine = s.person("ann@example.com", time::now_ms());
    let other = s.login("ann@example.com", PASSWORD).await.cookie();
    let cookie = [("cookie", mine.as_str())];
    let r = s
        .post("/ui/api/account/logout-others", &cookie, &json!({}))
        .await;
    assert_eq!(r.status, 204);
    assert_eq!(s.get("/ui/api/me", &[("cookie", &other)]).await.status, 401);
    let r = s.post("/ui/api/logout", &cookie, &json!({})).await;
    assert_eq!(r.status, 204);
    assert!(
        r.header("set-cookie").ends_with("Max-Age=0"),
        "{}",
        r.header("set-cookie")
    );
    assert_eq!(s.get("/ui/api/me", &cookie).await.status, 401);
}

#[tokio::test]
async fn every_ui_response_carries_the_headers_and_allows_no_origin() {
    let s = ui();
    let cases: [(Request<Body>, u16); 6] = [
        (
            request("GET", "/ui/", &[]).body(Body::empty()).unwrap(),
            200,
        ),
        (request("GET", "/ui", &[]).body(Body::empty()).unwrap(), 308),
        (
            request("GET", "/ui/assets/gone.js", &[])
                .body(Body::empty())
                .unwrap(),
            404,
        ),
        (
            request("GET", "/ui/api/me", &[])
                .body(Body::empty())
                .unwrap(),
            401,
        ),
        (
            request("GET", "/ui/", &[("host", "evil.example")])
                .body(Body::empty())
                .unwrap(),
            403,
        ),
        (
            Request::post("/ui/api/login")
                .header("host", "davy.example")
                .body(Body::empty())
                .unwrap(),
            403,
        ),
    ];
    for (req, status) in cases {
        let uri = req.uri().to_string();
        let r = s.send(req).await;
        assert_eq!(r.status, status, "{uri}");
        assert_eq!(r.header("content-security-policy"), super::CSP, "{uri}");
        assert_eq!(r.header("x-content-type-options"), "nosniff", "{uri}");
        assert_eq!(r.header("referrer-policy"), "same-origin", "{uri}");
        assert_eq!(
            r.header("cross-origin-opener-policy"),
            "same-origin",
            "{uri}"
        );
        assert!(
            r.header("permissions-policy").contains("camera=()"),
            "{uri}"
        );
        assert_eq!(r.header("cache-control"), "no-store", "{uri}");
        assert!(
            r.headers.get("access-control-allow-origin").is_none(),
            "{uri}"
        );
    }
}

#[tokio::test]
async fn ui_pages_fall_back_to_the_app_and_asset_misses_are_404() {
    let s = ui();
    let index = assets::FILES.iter().find(|(n, _)| *n == "index.html");
    for uri in ["/ui/", "/ui/issues/5", "/ui/welcome"] {
        let r = s
            .send(request("GET", uri, &[]).body(Body::empty()).unwrap())
            .await;
        assert_eq!(
            (r.status, r.header("content-type")),
            (200, "text/html; charset=utf-8"),
            "{uri}"
        );
        let page = s
            .app
            .clone()
            .oneshot(request("GET", uri, &[]).body(Body::empty()).unwrap());
        let bytes = to_bytes(page.await.unwrap().into_body(), 1 << 20)
            .await
            .unwrap();
        let expected = index.map_or(assets::NO_UI.as_bytes(), |(_, b)| *b);
        assert_eq!(&bytes[..], expected, "{uri}");
    }
    let r = s.get("/ui", &[]).await;
    assert_eq!((r.status, r.header("location")), (308, "/ui/"));
    assert_eq!(s.get("/ui/assets/gone.js", &[]).await.status, 404);
    let root = s.send(Request::get("/").body(Body::empty()).unwrap()).await;
    assert_eq!((root.status, root.header("location")), (303, "/ui/"));
}

#[path = "issues_tests.rs"]
mod issues;

#[path = "logs_tests.rs"]
mod logs;

#[path = "uptime_tests.rs"]
mod uptime;

#[path = "alerts_tests.rs"]
mod alerts;

#[path = "connect_tests.rs"]
mod connect;
