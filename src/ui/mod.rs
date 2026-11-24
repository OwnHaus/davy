//! The web UI: the pages under `/ui/` and the JSON API under `/ui/api/`.
//! Every `/ui/api/` route but login, logout and welcome asks [`Accounts`]
//! for the person first. Writes need JSON and the exact public origin.

mod account;
mod alerts;
mod assets;
mod connect;
pub mod issues;
mod logs;
mod monitors;
mod retention;
pub mod session;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod types;

use crate::{
    accounts::{Accounts, Person},
    http::{self, Failure, body::Budget, json_response},
    store,
};
use axum::{
    Extension, Router,
    extract::{Request, State},
    http::{HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Redirect, Response},
    routing::{any, get},
};
use issues::Reads;
use serde_json::json;
use session::Local;
use std::{path::Path, sync::Arc, time::Duration};

pub const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; font-src 'self'; connect-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'";

/// The routes under `/ui` of the server on `data`. `writes` is the writer's
/// UI queue, `budget` the memory budget of ingest.
pub fn serve(
    data: &Path,
    writes: store::Sender,
    budget: Budget,
    public_url: &url::Url,
    retention: crate::retention::Defaults,
) -> anyhow::Result<Router> {
    let local = Local::new(store::open(data)?, writes.clone());
    let reads = Reads::new(store::open(data)?, data.to_owned(), budget, writes);
    Ok(router(
        Arc::new(local),
        Arc::new(reads),
        public_url,
        retention,
    ))
}

/// The routes under `/ui`, for the people of `local`, reading issues from
/// `reads`, reached at `public_url`. Projects without a retention of their
/// own keep what `retention` says.
pub fn router(
    local: Arc<Local>,
    reads: Arc<Reads>,
    public_url: &url::Url,
    retention: crate::retention::Defaults,
) -> Router {
    let accounts: Arc<dyn Accounts> = Arc::clone(&local) as Arc<dyn Accounts>;
    let guard = Arc::new(Guard::new(public_url));
    protected(Arc::clone(&local), reads, public_url, retention)
        .route_layer(middleware::from_fn_with_state(accounts, require))
        .merge(account::open(local))
        .route("/ui", get(|| async { Redirect::permanent("/ui/") }))
        .route("/ui/", get(assets::page))
        .route("/ui/{*path}", get(assets::page))
        .layer(middleware::from_fn_with_state(guard, check))
        .layer(middleware::from_fn(headers))
}

/// The `/ui/api/` routes that need a person, each with the one method it
/// answers. A test sends each of them without a session.
fn protected(
    local: Arc<Local>,
    reads: Arc<Reads>,
    public_url: &url::Url,
    retention: crate::retention::Defaults,
) -> Router {
    Router::new()
        .route("/ui/api/me", get(me))
        .merge(retention::routes(Arc::clone(&reads), retention))
        .merge(monitors::routes(Arc::clone(&reads)))
        .merge(alerts::routes(Arc::clone(&reads)))
        .merge(connect::routes(Arc::clone(&reads), public_url))
        .merge(issues::routes(reads))
        .route(
            "/ui/api/{*rest}",
            any(|| async { refuse(StatusCode::NOT_FOUND, "no such route") }),
        )
        .merge(account::protected(local))
}

#[cfg(test)]
pub(crate) const PROTECTED: &[(&str, &str)] = &[
    ("GET", "/ui/api/me"),
    ("POST", "/ui/api/account/password"),
    ("POST", "/ui/api/account/logout-others"),
    ("GET", "/ui/api/projects"),
    ("GET", "/ui/api/issues"),
    ("POST", "/ui/api/issues/status"),
    ("GET", "/ui/api/issues/1"),
    ("GET", "/ui/api/issues/1/events"),
    ("GET", "/ui/api/issues/1/event"),
    ("GET", "/ui/api/issues/1/logs"),
    ("GET", "/ui/api/logs"),
    ("GET", "/ui/api/logs/histogram"),
    ("GET", "/ui/api/monitors"),
    ("GET", "/ui/api/monitors/1"),
    ("GET", "/ui/api/channels"),
    ("GET", "/ui/api/notifications"),
    ("GET", "/ui/api/connect"),
    ("GET", "/ui/api/setup"),
    ("GET", "/ui/api/retention"),
    ("POST", "/ui/api/retention"),
    ("GET", "/ui/api/nowhere"),
];

async fn me(Extension(person): Extension<Person>) -> Result<Response, Refusal> {
    json(&account::Me {
        email: person.email.as_str().to_owned(),
    })
}

fn json<T: serde::Serialize>(value: &T) -> Result<Response, Refusal> {
    let body = serde_json::to_vec(value).map_err(http::internal)?;
    Ok(([(header::CONTENT_TYPE, "application/json")], body).into_response())
}

async fn require(
    State(accounts): State<Arc<dyn Accounts>>,
    mut req: Request,
    next: Next,
) -> Response {
    match accounts.person(req.headers()) {
        Ok(Some(person)) => {
            req.extensions_mut().insert(person);
            next.run(req).await
        }
        Ok(None) => refuse(StatusCode::UNAUTHORIZED, "log in first"),
        Err(e) => Refusal::from(http::internal(e)).into_response(),
    }
}

/// Who may send what: the hosts a request may name, against DNS rebinding,
/// and the one origin a write may come from, against requests from other
/// sites and other ports of the same host.
struct Guard {
    hosts: [String; 4],
    origin: String,
}

impl Guard {
    fn new(public_url: &url::Url) -> Self {
        let public_host = public_url.host_str().map_or("localhost", |host| {
            host.trim_start_matches('[').trim_end_matches(']')
        });
        Self {
            hosts: ["localhost", "127.0.0.1", "::1", public_host].map(str::to_owned),
            origin: public_url.origin().ascii_serialization(),
        }
    }

    fn host_allowed(&self, req: &Request) -> bool {
        let mut hosts = req.headers().get_all(header::HOST).iter();
        let (host, None) = (hosts.next(), hosts.next()) else {
            return false;
        };
        let named = req
            .uri()
            .authority()
            .map(|a| a.host().to_owned())
            .or_else(|| {
                let value = host?.to_str().ok()?;
                let authority: axum::http::uri::Authority = value.parse().ok()?;
                Some(authority.host().to_owned())
            });
        named.is_some_and(|host| {
            let host = host.trim_start_matches('[').trim_end_matches(']');
            self.hosts.iter().any(|h| h.eq_ignore_ascii_case(host))
        })
    }

    fn write_allowed(&self, req: &Request) -> bool {
        let h = req.headers();
        let json = h
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next())
            .is_some_and(|media| media.trim().eq_ignore_ascii_case("application/json"));
        let origin = h
            .get(header::ORIGIN)
            .is_some_and(|o| o.as_bytes() == self.origin.as_bytes());
        json && origin
    }
}

async fn check(State(guard): State<Arc<Guard>>, req: Request, next: Next) -> Response {
    if !guard.host_allowed(&req) {
        return refuse(
            StatusCode::FORBIDDEN,
            "the Host header names another server",
        );
    }
    let write = !matches!(*req.method(), Method::GET | Method::HEAD);
    if write && !guard.write_allowed(&req) {
        let why = format!(
            "a write needs Content-Type: application/json and Origin: {}",
            guard.origin
        );
        return refuse(StatusCode::FORBIDDEN, &why);
    }
    next.run(req).await
}

/// What every `/ui/` response carries. The ingest routes allow any origin,
/// these allow none.
async fn headers(req: Request, next: Next) -> Response {
    let path = req.uri().path();
    let no_store = path.starts_with("/ui/api/") || path == "/ui/welcome";
    let mut response = next.run(req).await;
    let h = response.headers_mut();
    let fixed = [
        (header::CONTENT_SECURITY_POLICY, CSP),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::REFERRER_POLICY, "same-origin"),
        (
            header::HeaderName::from_static("cross-origin-opener-policy"),
            "same-origin",
        ),
        (
            header::HeaderName::from_static("permissions-policy"),
            "camera=(), microphone=(), geolocation=()",
        ),
    ];
    for (name, value) in fixed {
        h.insert(name, HeaderValue::from_static(value));
    }
    if no_store {
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    h.remove(header::ACCESS_CONTROL_ALLOW_ORIGIN);
    response
}

/// An error of the UI API, as `{"error": "..."}`.
#[derive(Debug)]
pub struct Refusal {
    status: StatusCode,
    message: String,
    retry_after: Option<Duration>,
}

impl Refusal {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            retry_after: None,
        }
    }

    fn bad(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    /// `429` with `Retry-After` in whole seconds, rounded up.
    fn wait(wait: Duration, message: impl Into<String>) -> Self {
        Self {
            retry_after: Some(wait),
            ..Self::new(StatusCode::TOO_MANY_REQUESTS, message)
        }
    }
}

fn refuse(status: StatusCode, message: &str) -> Response {
    Refusal::new(status, message).into_response()
}

impl From<Failure> for Refusal {
    fn from((status, message): Failure) -> Self {
        Self::new(status, message)
    }
}

impl From<anyhow::Error> for Refusal {
    fn from(e: anyhow::Error) -> Self {
        http::internal(e).into()
    }
}

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        let message = if self.status == StatusCode::INTERNAL_SERVER_ERROR {
            report(&self.message);
            "internal error, the server log says more"
        } else {
            &self.message
        };
        let mut response = json_response(&json!({ "error": message }));
        *response.status_mut() = self.status;
        if let Some(wait) = self.retry_after {
            let secs = wait.as_secs() + u64::from(wait.subsec_nanos() > 0);
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from(secs.max(1)));
        }
        response
    }
}

/// The text of an internal error goes to the log, not to a browser that may
/// not even have logged in.
#[expect(
    clippy::print_stderr,
    reason = "the server has no logger yet, stderr is its log"
)]
fn report(message: &str) {
    eprintln!("ui: {message}");
}

/// Queues `change` on the writer's UI queue and waits until it commits.
/// The inner error is the reason the change did not apply.
async fn write(
    writes: &store::Sender,
    change: crate::accounts::store::Change,
) -> Result<Result<(), String>, Refusal> {
    use crate::accounts::store::{LINK_USED, PASSWORD_CHANGED};
    let known = |why: &str| why == PASSWORD_CHANGED || why == LINK_USED;
    submit(writes, store::Job::Account(change), known).await
}

/// Queues `job` on the writer's UI queue and waits until it commits. The
/// inner error is a reason the job did not apply that `known` accepts as
/// a person's mistake rather than a fault of the server.
async fn submit(
    writes: &store::Sender,
    job: store::Job,
    known: impl Fn(&str) -> bool,
) -> Result<Result<(), String>, Refusal> {
    match store::submit(writes, job).await {
        Ok(()) => Ok(Ok(())),
        Err(store::Refused::Failed(why)) if known(&why) => Ok(Err(why)),
        Err(store::Refused::Full) => Err(Refusal {
            retry_after: Some(Duration::from_secs(1)),
            ..Refusal::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "the server is busy, try again",
            )
        }),
        Err(e @ (store::Refused::Stopped | store::Refused::Failed(_))) => {
            Err(http::internal(e).into())
        }
    }
}
