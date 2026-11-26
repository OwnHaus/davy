use crate::{
    envelope::{self, Disposition, Envelope, ItemKind, disposition},
    event::{ErrorEvent, EventId},
    heap,
    items::{SdkItem, client_report, session, user_feedback},
    logs::{
        LogRecord,
        store::{LogQueue, QueueError},
    },
    monitor::{self, HeartbeatToken},
    quota::Limits,
    sourcemaps::{self, symbolicate},
    store::{self, Job},
    time,
    usage::{self, Count, Signal},
};
use axum::{
    Router,
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
    sync::{Arc, Mutex, RwLock},
};

pub mod body;
mod heartbeat;
mod limit;
mod logs;
mod sdk;
mod serve;
mod vercel;
use crate::scrub::{Item, Policy};
use heartbeat::{fail, ping};
use logs::{ingest_logs, logtail_ingest, otlp_ingest};
use sdk::{check_in, scrubbed, scrubbed_logs, sdk_item};
pub use serve::serve;

/// Sentry's limit for one event or log item, so a client that works with Sentry fits.
const MAX_ITEM: usize = 1024 * 1024;
/// Log records one request may send. Memory is paid for already, so this
/// keeps one client from taking the whole log queue at once.
const MAX_RECORDS: usize = 10_000;
/// Client reports and feedback read from one envelope. An SDK sends one per
/// envelope, and the rest of a crafted one is skipped.
const MAX_SDK_ITEMS: usize = 10;
pub const RATE: &str = "86400:transaction;span;replay;profile;profile_chunk:organization";
/// What a request past a daily quota is told.
const OVER_QUOTA: &str = "over the project's daily quota";

pub struct Keys {
    db: Mutex<rusqlite::Connection>,
    known: RwLock<HashMap<String, i64>>,
    policies: Arc<crate::scrub::settings::Cache>,
}
impl Keys {
    pub fn new(db: rusqlite::Connection) -> Self {
        Self {
            db: Mutex::new(db),
            known: RwLock::default(),
            policies: Arc::default(),
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
    /// Not cached, so `davy project vercel` applies to the next request.
    fn vercel_secret(&self, project: i64) -> Result<Option<String>, Failure> {
        let db = self.db.lock().map_err(internal)?;
        crate::vercel::secret(&db, project).map_err(internal)
    }
    fn policy(&self, project: i64) -> Result<Arc<Policy>, Failure> {
        self.policies.get(&self.db, project).map_err(internal)
    }
    /// The policies ingest uses, which the scrub API updates when it
    /// stores one.
    pub fn policies(&self) -> Arc<crate::scrub::settings::Cache> {
        Arc::clone(&self.policies)
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
    pub sender: store::Sender,
    pub logs: LogQueue,
    pub sourcemaps: Arc<sourcemaps::Store>,
    /// The memory request bodies may hold while they are read and parsed.
    pub budget: body::Budget,
    /// What ingest accepted, until the next flush.
    pub usage: usage::Counter,
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
        .route(
            "/",
            get(|| async { axum::response::Redirect::to("/ui/") })
                .post(logtail_ingest)
                .options(preflight),
        )
        .route("/v1/logs", post(otlp_ingest).options(preflight))
        .route("/vercel/logs", post(vercel::ingest))
        .route("/api/heartbeat/{token}", get(ping).post(ping))
        .route("/api/heartbeat/{token}/fail", get(fail).post(fail))
        .with_state(app)
        .layer(middleware::from_fn(headers))
}
/// Outside `/ui/` Davy serves no pages, so a browser may not sniff, run,
/// frame or cache anything it sends. `/ui/` sets its own, looser headers,
/// and only fills in what it left out.
async fn protect(req: Request, next: Next) -> Response {
    let ui = req.uri().path() == "/ui" || req.uri().path().starts_with("/ui/");
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
        let value = HeaderValue::from_static(value);
        if ui {
            h.entry(name).or_insert(value);
        } else {
            h.insert(name, value);
        }
    }
    response
}
async fn headers(req: Request, next: Next) -> Response {
    let mut response = next.run(req).await;
    let throttled = response.status() == StatusCode::TOO_MANY_REQUESTS;
    let h = response.headers_mut();
    let fixed = [
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
    limit::merge(h, throttled);
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
    counted(app, project, req, false).await
}
async fn legacy(State(app): State<App>, Path(project): Path<i64>, req: Request) -> Response {
    counted(app, project, req, true).await
}
/// [`ingest`], with a `429` of a full body budget or queue counted as busy
/// for the project once its key picked it.
async fn counted(app: App, project: i64, req: Request, legacy: bool) -> Response {
    let mut known = None;
    let result = ingest(&app, project, req, legacy, &mut known).await;
    limit::busy(&app, known, result).unwrap_or_else(IntoResponse::into_response)
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
    /// Client reports and user feedback.
    items: Vec<SdkItem>,
    /// The first check-in, as Sentry allows one per envelope.
    check_in: Option<monitor::checkin::Item>,
    /// Records and bytes of what was parsed, counted once it is queued.
    tally: usage::Tally,
    /// Events and log records skipped unparsed past a daily quota, in the
    /// order of [`crate::quota::capped`].
    skipped: [u64; 2],
}
impl Parsed {
    fn new(id: Option<EventId>) -> Self {
        Self {
            id,
            event: None,
            logs: Vec::new(),
            items: Vec::new(),
            check_in: None,
            tally: usage::Tally::default(),
            skipped: [0; 2],
        }
    }
    /// Whether nothing is left to queue.
    fn is_empty(&self) -> bool {
        self.event.is_none()
            && self.logs.is_empty()
            && self.items.is_empty()
            && self.check_in.is_none()
    }
}
/// The place in [`crate::quota::capped`] of an item `limits` refuses, which
/// is skipped before it is parsed.
fn blocked(d: &Disposition, limits: &Limits) -> Option<usize> {
    let signal = match d {
        Disposition::StoreError => Signal::Errors,
        Disposition::StoreLogs => Signal::Logs,
        Disposition::SdkItem | Disposition::CheckIn | Disposition::Drop => return None,
    };
    limits
        .blocks(signal)
        .then(|| crate::quota::capped(signal))
        .flatten()
}
fn too_many_records() -> Failure {
    (
        StatusCode::PAYLOAD_TOO_LARGE,
        format!("more than {MAX_RECORDS} log records in one request"),
    )
}
fn event(
    json: &[u8],
    maps: &sourcemaps::Store,
    held: &mut Held,
    policy: &Policy,
) -> Result<ErrorEvent, Failure> {
    // The tree, and the event built from it.
    held.take(2 * heap::json(json, 0).map_err(bad)?)?;
    let mut value = serde_json::from_slice(json).map_err(bad)?;
    // A stored map that no longer reads, or a parse that waits too long,
    // leaves frames minified. The event still counts.
    let deadline = sourcemaps::parse_deadline();
    let moved = symbolicate(
        &mut value,
        |id| maps.build(id, deadline).ok().flatten(),
        |added| held.take(2 * added),
    )?;
    scrubbed(held, |meter| policy.item(&mut value, Item::Event, meter))?;
    Ok(ErrorEvent {
        symbolicated: moved > 0,
        ..ErrorEvent::parse(value).map_err(bad)?
    })
}
fn parse_sentry(
    bytes: &[u8],
    project: i64,
    legacy: bool,
    limits: &Limits,
    maps: &sourcemaps::Store,
    held: &mut Held,
    policy: &Policy,
) -> Result<Parsed, Failure> {
    if legacy {
        let mut parsed = Parsed::new(None);
        if bytes.len() <= MAX_ITEM {
            parsed.tally.add(Signal::Errors, Count::new(1, bytes.len()));
            parsed.event = Some(event(bytes, maps, held, policy)?);
        }
        return Ok(parsed);
    }
    let envelope = Envelope::parse(bytes).map_err(bad)?;
    let observed_at = time::now_ns();
    let mut parsed = Parsed::new(envelope.header.event_id);
    for item in envelope
        .items
        .into_iter()
        .filter(|i| i.payload.len() <= MAX_ITEM)
    {
        let d = disposition(&item.kind);
        if let Some(i) = blocked(&d, limits) {
            // Sentry keeps one event an envelope, so one is refused at most.
            let n = &mut parsed.skipped[i];
            *n = if i == 0 {
                1
            } else {
                n.saturating_add(crate::logs::sentry::count(item.payload))
            };
            continue;
        }
        match d {
            Disposition::StoreError if parsed.event.is_none() => {
                parsed.event = Some(event(item.payload, maps, held, policy)?);
                let count = Count::new(1, item.payload.len());
                parsed.tally.add(Signal::Errors, count);
            }
            Disposition::SdkItem if parsed.items.len() < MAX_SDK_ITEMS => {
                let one = sdk_item(&item, held, policy)?;
                if let Some(signal) = one.as_ref().and_then(signal_of) {
                    parsed.tally.add(signal, Count::new(1, item.payload.len()));
                }
                parsed.items.extend(one);
            }
            Disposition::CheckIn if parsed.check_in.is_none() => {
                let one = check_in(item.payload, held, policy)?;
                if one.is_ok() {
                    let count = Count::new(1, item.payload.len());
                    parsed.tally.add(Signal::Checkins, count);
                }
                parsed.check_in = Some(one);
            }
            Disposition::StoreError
            | Disposition::Drop
            | Disposition::SdkItem
            | Disposition::CheckIn => {}
            Disposition::StoreLogs => {
                // An item that does not parse is dropped, as Sentry drops it.
                let Ok(cost) = crate::logs::sentry::cost(item.payload) else {
                    continue;
                };
                held.take(cost)?;
                let records = crate::logs::sentry::parse(item.payload, project, observed_at)
                    .unwrap_or_default();
                if parsed.logs.len() + records.len() > MAX_RECORDS {
                    return Err(too_many_records());
                }
                // The tree is gone, and only the records stay.
                let kept = records.iter().map(heap::of_record).sum::<usize>();
                held.release(cost.saturating_sub(kept));
                if !records.is_empty() {
                    let count = Count::new(records.len(), item.payload.len());
                    parsed.tally.add(Signal::Logs, count);
                }
                parsed.logs.extend(records);
            }
        }
    }
    Ok(parsed)
}
/// The signal an SDK item counts as. Client reports describe what the SDK
/// dropped, so they count as none.
fn signal_of(item: &SdkItem) -> Option<Signal> {
    match item {
        SdkItem::ClientReport(_) => None,
        SdkItem::UserFeedback(_) => Some(Signal::Feedback),
        SdkItem::Session(_) | SdkItem::Sessions(_) => Some(Signal::Sessions),
    }
}
/// Takes a Sentry request, and sets `known` once its key picked the
/// project.
async fn ingest(
    app: &App,
    project: i64,
    req: Request,
    legacy: bool,
    known: &mut Option<i64>,
) -> Result<Response, Failure> {
    let (parts, raw) = req.into_parts();
    let presented = key(&parts.headers, parts.uri.query(), None);
    let encoding = encoding(&parts.headers);
    let mut held = app.budget.hold();
    // Only an envelope can carry its key in the body, in the DSN of its header line.
    let (bytes, limits) = if presented.is_none() && !legacy {
        let bytes = body::read_envelope(raw, &mut held, &encoding, |header| {
            let dsn = header.dsn.as_deref();
            let key = key(&HeaderMap::new(), None, dsn);
            authorize(&app.keys, project, key.as_deref())?;
            *known = Some(project);
            Ok(())
        })
        .await?;
        (bytes, limit::limits(app, project))
    } else {
        authorize(&app.keys, project, presented.as_deref())?;
        *known = Some(project);
        let limits = limit::limits(app, project);
        // A legacy request is one event, refused before its body is read.
        if legacy && let Some(l) = limit::refused(app, project, Signal::Errors, &limits) {
            let drained = body::drain(raw, &parts.headers, body::COMPRESSED).await;
            return Ok(body::closing(l.refuse(OVER_QUOTA), &drained));
        }
        (body::read(raw, &mut held).await?, limits)
    };
    let policy = app.keys.policy(project)?;
    let maps = Arc::clone(&app.sourcemaps);
    let (parsed, held) = blocking(move || {
        let body = body::decode(bytes, &encoding, &mut held)?;
        let mut parsed = parse_sentry(&body, project, legacy, &limits, &maps, &mut held, &policy)?;
        scrubbed_logs(&mut held, |meter| policy.logs(&mut parsed.logs, meter))?;
        let capacity = body.capacity();
        drop(body);
        held.release(capacity);
        Ok((parsed, held))
    })
    .await?;
    limit::count_skipped(app, project, parsed.skipped);
    let limited = limit::Limited::of(&limits);
    if let Some(l) = &limited
        && parsed.is_empty()
        && parsed.skipped.iter().any(|n| *n > 0)
    {
        return Ok(l.refuse(OVER_QUOTA));
    }
    let id = parsed
        .id
        .clone()
        .or_else(|| parsed.event.as_ref().map(|e| e.event_id.clone()))
        .unwrap_or_else(EventId::fresh);
    accept(app, project, parsed, held).await?;
    let mut response = json_response(&json!({"id":id.0}));
    // The SDK stops sending what is refused, though this request is taken.
    if let Some(l) = limited {
        l.set(response.headers_mut(), false);
    }
    Ok(response)
}
/// Queues what a Sentry request carried, and counts each part once it is
/// queued.
async fn accept(app: &App, project: i64, parsed: Parsed, held: Held) -> Result<(), Failure> {
    let tally = parsed.tally;
    let count = |signal| app.usage.add(project, signal, tally.get(signal));
    if !parsed.logs.is_empty() {
        app.logs.push(parsed.logs)?;
        count(Signal::Logs);
    }
    if let Some(event) = parsed.event {
        let events = vec![event];
        let job = Job::Ingest {
            project,
            events,
            held,
        };
        let queued = store::queue(&app.sender, job)?;
        count(Signal::Errors);
        queued.applied().await?;
    }
    // After the event, so a full queue costs a report or feedback, not an error.
    if !parsed.items.is_empty() {
        let job = Job::Items {
            project,
            at: time::now_ms(),
            items: parsed.items,
        };
        let queued = store::queue(&app.sender, job)?;
        count(Signal::Sessions);
        count(Signal::Feedback);
        queued.applied().await?;
    }
    if let Some(item) = parsed.check_in {
        let at = time::now_ms();
        let queued = store::queue(&app.sender, Job::CheckIn { project, at, item })?;
        count(Signal::Checkins);
        queued.applied().await?;
    }
    Ok(())
}
#[cfg(test)]
#[path = "http/key_tests.rs"]
mod key_tests;
