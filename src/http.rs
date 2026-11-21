use crate::{
    alert,
    envelope::{Disposition, Envelope, disposition},
    event::{ErrorEvent, EventId},
    heap,
    logs::{
        self, LogRecord, logtail, otlp,
        store::{LogQueue, LogWriter, QueueError},
    },
    mcp,
    monitor::{self, HeartbeatToken},
    net,
    sourcemaps::{self, symbolicate},
    store::{self, Job, Write},
    time,
    token::{Token, bearer},
    upload,
};
use axum::{
    Router,
    body::Body,
    extract::{Path, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use body::Held;
use serde_json::json;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, RwLock, mpsc},
    time::Duration,
};

pub mod body;

/// Sentry's limit for one event or log item, so a client that works with Sentry fits.
const MAX_ITEM: usize = 1024 * 1024;
/// Log records one request may send. Memory is paid for already, so this
/// keeps one client from taking the whole log queue at once.
const MAX_RECORDS: usize = 10_000;
/// How late a missed heartbeat can be noticed.
const SWEEP_EVERY: Duration = Duration::from_secs(10);
/// How late a due notification can be sent.
const NOTIFY_EVERY: Duration = Duration::from_secs(1);
/// How late a due HTTP check can start.
const CHECK_EVERY: Duration = Duration::from_secs(1);
pub const RATE: &str = "86400:transaction;span;replay;profile;profile_chunk:organization";

pub struct Keys {
    db: Mutex<rusqlite::Connection>,
    known: RwLock<HashMap<String, i64>>,
}
impl Keys {
    pub fn new(db: rusqlite::Connection) -> Self {
        Self {
            db: Mutex::new(db),
            known: RwLock::default(),
        }
    }
    fn project_for(&self, key: &str) -> Result<Option<i64>, Failure> {
        if let Some(id) = self.known.read().ok().and_then(|k| k.get(key).copied()) {
            return Ok(Some(id));
        }
        let found = {
            let db = self.db.lock().map_err(internal)?;
            store::project_for_key(&db, key).map_err(internal)?
        };
        if let (Some(id), Ok(mut known)) = (found, self.known.write()) {
            known.insert(key.to_owned(), id);
        }
        Ok(found)
    }
    /// Not cached like project keys, because `davy monitor remove` can delete
    /// a monitor while the server runs.
    fn monitor_for(&self, token: &HeartbeatToken) -> Result<Option<i64>, Failure> {
        let db = self.db.lock().map_err(internal)?;
        monitor::store::by_token(&db, token).map_err(internal)
    }
    fn authenticate(&self, key: Option<&str>) -> Result<i64, Failure> {
        let key = key
            .filter(|s| !s.is_empty())
            .ok_or_else(|| (StatusCode::UNAUTHORIZED, "missing key".to_owned()))?;
        self.project_for(key)?
            .ok_or_else(|| (StatusCode::FORBIDDEN, "invalid project key".into()))
    }
}

#[derive(Clone)]
pub struct App {
    pub keys: Arc<Keys>,
    pub sender: mpsc::SyncSender<Write>,
    pub logs: LogQueue,
    pub sourcemaps: Arc<sourcemaps::Store>,
    /// The memory request bodies may hold while they are read and parsed.
    pub budget: body::Budget,
}
/// Ingest, the `admin` routes, and a 404 for anything else, all with
/// [`protect`]ive headers.
pub fn router(app: App, admin: Router) -> Router {
    ingest_routes(app)
        .merge(admin)
        .layer(middleware::from_fn(protect))
}
fn ingest_routes(app: App) -> Router {
    let preflight = || async { StatusCode::NO_CONTENT };
    Router::new()
        .route(
            "/api/{project}/envelope/",
            post(envelope).options(preflight),
        )
        .route("/api/{project}/store/", post(legacy).options(preflight))
        .route("/", post(logtail_ingest).options(preflight))
        .route("/v1/logs", post(otlp_ingest).options(preflight))
        .route("/api/heartbeat/{token}", get(ping).post(ping))
        .route("/api/heartbeat/{token}/fail", get(fail).post(fail))
        .with_state(app)
        .layer(middleware::from_fn(headers))
}
/// Davy serves no pages, so a browser may not sniff, run, frame or cache
/// anything it sends.
async fn protect(req: Request, next: Next) -> Response {
    let mut response = next.run(req).await;
    let h = response.headers_mut();
    let fixed = [
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (
            header::CONTENT_SECURITY_POLICY,
            "default-src 'none'; frame-ancestors 'none'",
        ),
        (header::CACHE_CONTROL, "no-store"),
    ];
    for (name, value) in fixed {
        h.insert(name, HeaderValue::from_static(value));
    }
    response
}
async fn headers(req: Request, next: Next) -> Response {
    let mut response = next.run(req).await;
    let throttled = response.status() == StatusCode::TOO_MANY_REQUESTS;
    let h = response.headers_mut();
    let fixed = [
        ("x-sentry-rate-limits", RATE),
        ("access-control-allow-origin", "*"),
        ("access-control-allow-methods", "POST, OPTIONS"),
        (
            "access-control-allow-headers",
            "authorization, content-type, content-encoding, x-sentry-auth, sentry-trace, baggage",
        ),
        (
            "access-control-expose-headers",
            "X-Sentry-Rate-Limits, Retry-After",
        ),
    ];
    for (name, value) in fixed {
        h.insert(name, HeaderValue::from_static(value));
    }
    if throttled {
        h.insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
    }
    response
}
pub(crate) type Failure = (StatusCode, String);
/// A JSON response. axum's `Json` would turn on serde_json's `raw_value`,
/// under which an object whose first key is serde_json's private token
/// parses its string as a fresh JSON tree, unpriced and with its own depth
/// limit, so davy builds its JSON responses and requests itself.
pub(crate) fn json_response(v: &serde_json::Value) -> Response {
    ([(header::CONTENT_TYPE, "application/json")], v.to_string()).into_response()
}
pub(crate) fn bad(e: impl std::fmt::Display) -> Failure {
    (StatusCode::BAD_REQUEST, e.to_string())
}
pub(crate) fn internal(e: impl std::fmt::Display) -> Failure {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}
pub(crate) async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, Failure> + Send + 'static,
) -> Result<T, Failure> {
    tokio::task::spawn_blocking(f).await.map_err(internal)?
}
fn queue_full() -> Failure {
    (StatusCode::TOO_MANY_REQUESTS, "ingest queue full".into())
}
fn writer_stopped() -> Failure {
    (StatusCode::SERVICE_UNAVAILABLE, "writer stopped".into())
}
impl From<store::Refused> for Failure {
    fn from(e: store::Refused) -> Self {
        match e {
            store::Refused::Full => queue_full(),
            store::Refused::Stopped => writer_stopped(),
            store::Refused::Failed(e) => internal(e),
        }
    }
}
impl From<QueueError> for Failure {
    fn from(e: QueueError) -> Self {
        match e {
            QueueError::Full => queue_full(),
            QueueError::TooLarge => (
                StatusCode::PAYLOAD_TOO_LARGE,
                "log batch is larger than the whole ingest queue".into(),
            ),
            QueueError::Stopped => writer_stopped(),
        }
    }
}
fn header_str(headers: &HeaderMap, name: header::HeaderName) -> &str {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .trim()
}
fn encoding(headers: &HeaderMap) -> String {
    header_str(headers, header::CONTENT_ENCODING).to_ascii_lowercase()
}
fn key(headers: &HeaderMap, query: Option<&str>, dsn: Option<&str>) -> Option<String> {
    if let Some(h) = headers.get("x-sentry-auth") {
        let h = h
            .to_str()
            .unwrap_or("")
            .trim()
            .strip_prefix("Sentry ")
            .unwrap_or("");
        if let Some(key) = h.split(',').find_map(|v| {
            v.trim()
                .strip_prefix("sentry_key=")
                .map(|k| k.trim_matches('"').to_owned())
        }) {
            return Some(key);
        }
    }
    if let Some((_, v)) =
        url::form_urlencoded::parse(query.unwrap_or("").as_bytes()).find(|(k, _)| k == "sentry_key")
    {
        return Some(v.into_owned());
    }
    dsn.and_then(|s| url::Url::parse(s).ok())
        .map(|u| u.username().to_owned())
        .filter(|s| !s.is_empty())
}
async fn envelope(State(app): State<App>, Path(project): Path<i64>, req: Request) -> Response {
    ingest(app, project, req, false)
        .await
        .unwrap_or_else(IntoResponse::into_response)
}
async fn legacy(State(app): State<App>, Path(project): Path<i64>, req: Request) -> Response {
    ingest(app, project, req, true)
        .await
        .unwrap_or_else(IntoResponse::into_response)
}
fn authorize(keys: &Keys, project: i64, key: Option<&str>) -> Result<(), Failure> {
    if keys.authenticate(key)? == project {
        Ok(())
    } else {
        Err((StatusCode::FORBIDDEN, "invalid project key".into()))
    }
}
struct Parsed {
    id: Option<EventId>,
    /// Sentry allows one event per envelope. Later event items are dropped.
    event: Option<ErrorEvent>,
    logs: Vec<LogRecord>,
}
fn too_many_records() -> Failure {
    (
        StatusCode::PAYLOAD_TOO_LARGE,
        format!("more than {MAX_RECORDS} log records in one request"),
    )
}
fn event(json: &[u8], maps: &sourcemaps::Store, held: &mut Held) -> Result<ErrorEvent, Failure> {
    // The tree, and the event built from it.
    held.take(2 * heap::json(json, 0).map_err(bad)?)?;
    let mut value = serde_json::from_slice(json).map_err(bad)?;
    // A stored map that no longer reads leaves frames minified. The event still counts.
    symbolicate(
        &mut value,
        |id| maps.build(id).ok().flatten(),
        |added| held.take(2 * added),
    )?;
    ErrorEvent::parse(value).map_err(bad)
}
fn parse_sentry(
    bytes: &[u8],
    project: i64,
    legacy: bool,
    maps: &sourcemaps::Store,
    held: &mut Held,
) -> Result<Parsed, Failure> {
    if legacy {
        return Ok(Parsed {
            id: None,
            event: if bytes.len() > MAX_ITEM {
                None
            } else {
                Some(event(bytes, maps, held)?)
            },
            logs: Vec::new(),
        });
    }
    let envelope = Envelope::parse(bytes).map_err(bad)?;
    let observed_at = time::now_ns();
    let mut parsed = Parsed {
        id: envelope.header.event_id,
        event: None,
        logs: Vec::new(),
    };
    for item in envelope
        .items
        .into_iter()
        .filter(|i| i.payload.len() <= MAX_ITEM)
    {
        match disposition(&item.kind) {
            Disposition::StoreError if parsed.event.is_none() => {
                parsed.event = Some(event(item.payload, maps, held)?);
            }
            Disposition::StoreError | Disposition::Drop => {}
            Disposition::StoreLogs => {
                // An item that does not parse is dropped, as Sentry drops it.
                let Ok(cost) = logs::sentry::cost(item.payload) else {
                    continue;
                };
                held.take(cost)?;
                let records =
                    logs::sentry::parse(item.payload, project, observed_at).unwrap_or_default();
                if parsed.logs.len() + records.len() > MAX_RECORDS {
                    return Err(too_many_records());
                }
                // The tree is gone, and only the records stay.
                let kept = records.iter().map(heap::of_record).sum::<usize>();
                held.release(cost.saturating_sub(kept));
                parsed.logs.extend(records);
            }
        }
    }
    Ok(parsed)
}
async fn ingest(app: App, project: i64, req: Request, legacy: bool) -> Result<Response, Failure> {
    let (parts, raw) = req.into_parts();
    let presented = key(&parts.headers, parts.uri.query(), None);
    let encoding = encoding(&parts.headers);
    let mut held = app.budget.hold();
    // Only an envelope can carry its key in the body, in the DSN of its header line.
    let bytes = if presented.is_none() && !legacy {
        body::read_envelope(raw, &mut held, &encoding, |header| {
            let dsn = header.dsn.as_deref();
            authorize(
                &app.keys,
                project,
                key(&HeaderMap::new(), None, dsn).as_deref(),
            )
        })
        .await?
    } else {
        authorize(&app.keys, project, presented.as_deref())?;
        body::read(raw, &mut held).await?
    };
    let maps = Arc::clone(&app.sourcemaps);
    let (parsed, _held) = blocking(move || {
        let body = body::decode(bytes, &encoding, &mut held)?;
        let parsed = parse_sentry(&body, project, legacy, &maps, &mut held)?;
        let capacity = body.capacity();
        drop(body);
        held.release(capacity);
        Ok((parsed, held))
    })
    .await?;
    let id = parsed
        .id
        .or_else(|| parsed.event.as_ref().map(|e| e.event_id.clone()))
        .unwrap_or_else(EventId::fresh);
    if !parsed.logs.is_empty() {
        app.logs.push(parsed.logs)?;
    }
    if let Some(event) = parsed.event {
        let events = vec![event];
        store::submit(&app.sender, Job::Ingest { project, events }).await?;
    }
    Ok(json_response(&json!({"id":id.0})))
}
async fn ping(State(app): State<App>, Path(token): Path<String>) -> Result<&'static str, Failure> {
    heartbeat(&app, &token, false).await
}
async fn fail(State(app): State<App>, Path(token): Path<String>) -> Result<&'static str, Failure> {
    heartbeat(&app, &token, true).await
}
/// The token is the credential, so a malformed and an unknown one get the
/// same `404`.
async fn heartbeat(app: &App, token: &str, failed: bool) -> Result<&'static str, Failure> {
    let unknown = || {
        (
            StatusCode::NOT_FOUND,
            "no heartbeat with this token".to_owned(),
        )
    };
    let token = HeartbeatToken::parse(token).ok_or_else(unknown)?;
    let monitor = app.keys.monitor_for(&token)?.ok_or_else(unknown)?;
    let at = time::now_ms();
    store::submit(
        &app.sender,
        Job::Ping {
            monitor,
            at,
            failed,
        },
    )
    .await?;
    Ok("ok")
}
/// Reads, decompresses and parses a log body, which `parse` pays for, and
/// queues its records.
async fn ingest_logs(
    app: &App,
    headers: &HeaderMap,
    raw: Body,
    parse: impl FnOnce(&[u8], &mut Held) -> Result<Vec<LogRecord>, Failure> + Send + 'static,
) -> Result<(), Failure> {
    let mut held = app.budget.hold();
    let bytes = body::read(raw, &mut held).await?;
    let encoding = encoding(headers);
    let (records, _held) = blocking(move || {
        let body = body::decode(bytes, &encoding, &mut held)?;
        let records = parse(&body, &mut held)?;
        Ok((records, held))
    })
    .await?;
    if records.len() > MAX_RECORDS {
        return Err(too_many_records());
    }
    if !records.is_empty() {
        app.logs.push(records)?;
    }
    Ok(())
}
async fn logtail_ingest(State(app): State<App>, req: Request) -> Response {
    let run = async {
        let (parts, raw) = req.into_parts();
        let project = app.keys.authenticate(bearer(&parts.headers))?;
        let content_type = header_str(&parts.headers, header::CONTENT_TYPE).to_owned();
        ingest_logs(&app, &parts.headers, raw, move |body, held| {
            let refused = |e| (StatusCode::NOT_ACCEPTABLE, e);
            held.take(logtail::cost(body, &content_type).map_err(refused)?)?;
            logtail::parse(body, &content_type, project, time::now_ns()).map_err(refused)
        })
        .await?;
        Ok::<_, Failure>(StatusCode::ACCEPTED.into_response())
    };
    run.await.unwrap_or_else(IntoResponse::into_response)
}
async fn otlp_ingest(State(app): State<App>, req: Request) -> Response {
    let (parts, raw) = req.into_parts();
    let encoding =
        otlp::Encoding::from_content_type(header_str(&parts.headers, header::CONTENT_TYPE));
    let run = async {
        let project = app.keys.authenticate(bearer(&parts.headers))?;
        let encoding = encoding.ok_or_else(|| {
            (
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "expected application/x-protobuf or application/json".to_owned(),
            )
        })?;
        ingest_logs(&app, &parts.headers, raw, move |body, held| {
            held.take(otlp::cost(body, encoding).map_err(bad)?)?;
            let request = otlp::decode(body, encoding).map_err(bad)?;
            otlp::convert(request, project, time::now_ns(), |bytes| held.take(bytes))
        })
        .await?;
        Ok::<_, Failure>(encoding)
    };
    match (run.await, encoding) {
        (Ok(encoding), _) => (
            [(header::CONTENT_TYPE, encoding.content_type())],
            otlp::success(encoding),
        )
            .into_response(),
        (Err((status, message)), Some(encoding)) => (
            status,
            [(header::CONTENT_TYPE, encoding.content_type())],
            otlp::status(encoding, &message),
        )
            .into_response(),
        (Err(failure), None) => failure.into_response(),
    }
}
/// Serves ingest, and with a token `/mcp` and the source map upload.
/// `public_url` is where clients reach this server.
pub async fn serve(
    data: &std::path::Path,
    listen: &str,
    public_url: &url::Url,
    log_retention_days: u32,
    token: Option<Token>,
    smtp: Option<alert::mail::Smtp>,
) -> anyhow::Result<()> {
    let writer = store::Writer::start(store::open(data)?, 1024);
    let log_writer = LogWriter::start(logs::dir(data), log_retention_days)?;
    let app = App {
        keys: Arc::new(Keys::new(store::open(data)?)),
        sender: writer.sender.clone(),
        logs: log_writer.queue.clone(),
        sourcemaps: Arc::new(sourcemaps::Store::open(data)),
        budget: body::Budget::new(body::MEMORY),
    };
    let maps = Arc::clone(&app.sourcemaps);
    let admin = match token {
        Some(token) => mcp::router(
            data.to_owned(),
            writer.sender.clone(),
            token.clone(),
            public_url,
        )
        .merge(upload::router(maps, token, public_url)),
        None => Router::new(),
    };
    let routes = router(app, admin);
    let listener = tokio::net::TcpListener::bind(listen).await?;
    crate::ready::notify()?;
    let sweeper = tokio::spawn(store::sweep_every(writer.sender.clone(), SWEEP_EVERY));
    let checker = tokio::spawn(monitor::check::run(
        store::open(data)?,
        monitor::check::client(net::Reach::Any)?,
        writer.sender.clone(),
        CHECK_EVERY,
    ));
    let notifier = tokio::spawn(alert::deliver::run(
        store::open(data)?,
        alert::deliver::Transports::new(net::Reach::Any, smtp)?,
        writer.sender.clone(),
        NOTIFY_EVERY,
    ));
    #[cfg(unix)]
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let result = axum::serve(listener, routes)
        .with_graceful_shutdown(async move {
            #[cfg(unix)]
            tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} }
            #[cfg(not(unix))]
            if tokio::signal::ctrl_c().await.is_err() {
                std::future::pending::<()>().await;
            }
        })
        .await;
    // These tasks hold senders, and the writer thread ends only when every
    // sender is gone. A notice in flight is sent again on the next start.
    for (name, task) in [
        ("sweeper", sweeper),
        ("checker", checker),
        ("notifier", notifier),
    ] {
        task.abort();
        if let Err(e) = task.await
            && e.is_panic()
        {
            anyhow::bail!("{name} panicked");
        }
    }
    drop(writer.sender);
    drop(log_writer.queue);
    for (name, thread) in [("writer", writer.thread), ("log writer", log_writer.thread)] {
        thread.join().map_err(|panic| {
            let why = panic
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
                .unwrap_or("no message");
            anyhow::anyhow!("{name} panicked: {why}")
        })?;
    }
    result?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authentication_precedence() {
        let mut headers = HeaderMap::new();
        let dsn = Some("https://dsn-key@example.com/1");
        assert_eq!(key(&headers, None, dsn).as_deref(), Some("dsn-key"));
        assert_eq!(
            key(&headers, Some("sentry_key=query-key"), dsn).as_deref(),
            Some("query-key")
        );
        headers.insert(
            "x-sentry-auth",
            "Sentry sentry_version=7, sentry_key=header-key"
                .parse()
                .unwrap(),
        );
        assert_eq!(
            key(&headers, Some("sentry_key=query-key"), dsn).as_deref(),
            Some("header-key")
        );
    }
}
