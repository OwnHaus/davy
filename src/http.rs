use crate::{
    envelope::{Disposition, Envelope, disposition},
    event::{ErrorEvent, EventId},
    logs::{
        self, LogRecord, logtail, otlp,
        store::{LogQueue, LogWriter, QueueError},
    },
    mcp,
    store::{self, Job, Write},
    time,
    token::Token,
};
use axum::{
    Json, Router,
    body::{Bytes, to_bytes},
    extract::{Path, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::post,
};
use serde_json::json;
use std::{
    collections::HashMap,
    io::{Cursor, Read},
    sync::{Arc, Mutex, RwLock, mpsc},
};
const COMPRESSED: usize = 20 * 1024 * 1024;
const DECOMPRESSED: usize = 200 * 1024 * 1024;
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
}
pub fn router(app: App) -> Router {
    let preflight = || async { StatusCode::NO_CONTENT };
    Router::new()
        .route(
            "/api/{project}/envelope/",
            post(envelope).options(preflight),
        )
        .route("/api/{project}/store/", post(legacy).options(preflight))
        .route("/", post(logtail_ingest).options(preflight))
        .route("/v1/logs", post(otlp_ingest).options(preflight))
        .with_state(app)
        .layer(middleware::from_fn(headers))
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
type Failure = (StatusCode, String);
fn bad(e: impl std::fmt::Display) -> Failure {
    (StatusCode::BAD_REQUEST, e.to_string())
}
fn internal(e: impl std::fmt::Display) -> Failure {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, Failure> + Send + 'static,
) -> Result<T, Failure> {
    tokio::task::spawn_blocking(f).await.map_err(internal)?
}
fn queue<T>(sender: &mpsc::SyncSender<T>, value: T) -> Result<(), Failure> {
    sender.try_send(value).map_err(|e| match e {
        mpsc::TrySendError::Full(_) => (StatusCode::TOO_MANY_REQUESTS, "ingest queue full".into()),
        mpsc::TrySendError::Disconnected(_) => {
            (StatusCode::SERVICE_UNAVAILABLE, "writer stopped".into())
        }
    })
}
fn queue_logs(logs: &LogQueue, batch: Vec<LogRecord>) -> Result<(), Failure> {
    logs.push(batch).map_err(|e| match e {
        QueueError::Full => (StatusCode::TOO_MANY_REQUESTS, "ingest queue full".into()),
        QueueError::Stopped => (StatusCode::SERVICE_UNAVAILABLE, "writer stopped".into()),
    })
}
fn decode(bytes: &[u8], encoding: &str) -> Result<Vec<u8>, Failure> {
    let source = Cursor::new(bytes);
    let reader: Box<dyn Read + '_> = match encoding {
        "" | "identity" => Box::new(source),
        "gzip" => Box::new(flate2::read::GzDecoder::new(source)),
        "deflate" => Box::new(flate2::read::ZlibDecoder::new(source)),
        "br" => Box::new(brotli::Decompressor::new(source, 4096)),
        "zstd" => Box::new(zstd::stream::read::Decoder::new(source).map_err(bad)?),
        _ => return Err(bad("unsupported content encoding")),
    };
    let mut out = Vec::new();
    reader
        .take(DECOMPRESSED as u64 + 1)
        .read_to_end(&mut out)
        .map_err(bad)?;
    if out.len() > DECOMPRESSED {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            "decompressed body too large".into(),
        ));
    }
    Ok(out)
}
fn header_str(headers: &HeaderMap, name: header::HeaderName) -> &str {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .trim()
}
async fn body(headers: &HeaderMap, body: axum::body::Body) -> Result<Vec<u8>, Failure> {
    let bytes: Bytes = to_bytes(body, COMPRESSED).await.map_err(|e| {
        (
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("body too large: {e}"),
        )
    })?;
    let encoding = header_str(headers, header::CONTENT_ENCODING).to_ascii_lowercase();
    blocking(move || decode(&bytes, &encoding)).await
}
fn bearer(headers: &HeaderMap) -> Option<&str> {
    let value = header_str(headers, header::AUTHORIZATION);
    let (scheme, token) = value.split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then(|| token.trim())
        .filter(|t| !t.is_empty())
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
fn authorize(app: &App, project: i64, key: Option<&str>) -> Result<(), Failure> {
    if app.keys.authenticate(key)? == project {
        Ok(())
    } else {
        Err((StatusCode::FORBIDDEN, "invalid project key".into()))
    }
}
struct Parsed {
    id: Option<EventId>,
    dsn: Option<String>,
    events: Vec<ErrorEvent>,
    logs: Vec<LogRecord>,
}
fn parse_sentry(bytes: &[u8], project: i64, legacy: bool) -> Result<Parsed, Failure> {
    if legacy {
        let event = ErrorEvent::parse(serde_json::from_slice(bytes).map_err(bad)?).map_err(bad)?;
        return Ok(Parsed {
            id: None,
            dsn: None,
            events: vec![event],
            logs: Vec::new(),
        });
    }
    let envelope = Envelope::parse(bytes).map_err(bad)?;
    let observed_at = time::now_ns();
    let mut parsed = Parsed {
        id: envelope.header.event_id,
        dsn: envelope.header.dsn,
        events: Vec::new(),
        logs: Vec::new(),
    };
    for item in envelope.items {
        match disposition(&item.kind) {
            Disposition::StoreError => parsed.events.push(
                ErrorEvent::parse(serde_json::from_slice(&item.payload).map_err(bad)?)
                    .map_err(bad)?,
            ),
            Disposition::StoreLogs => {
                if let Ok(records) = logs::sentry::parse(&item.payload, project, observed_at) {
                    parsed.logs.extend(records);
                }
            }
            Disposition::Drop(_) => {}
        }
    }
    Ok(parsed)
}
async fn ingest(app: App, project: i64, req: Request, legacy: bool) -> Result<Response, Failure> {
    let (parts, raw) = req.into_parts();
    let early = key(&parts.headers, parts.uri.query(), None);
    if early.is_some() {
        authorize(&app, project, early.as_deref())?;
    }
    let bytes = body(&parts.headers, raw).await?;
    let parsed = blocking(move || parse_sentry(&bytes, project, legacy)).await?;
    if early.is_none() {
        authorize(
            &app,
            project,
            key(&HeaderMap::new(), None, parsed.dsn.as_deref()).as_deref(),
        )?;
    }
    let id = parsed
        .id
        .or_else(|| parsed.events.first().map(|e| e.event_id.clone()))
        .unwrap_or_else(EventId::fresh);
    if !parsed.logs.is_empty() {
        queue_logs(&app.logs, parsed.logs)?;
    }
    if !parsed.events.is_empty() {
        let (done, rx) = tokio::sync::oneshot::channel();
        queue(
            &app.sender,
            Write {
                job: Job::Ingest {
                    project,
                    events: parsed.events,
                },
                done,
            },
        )?;
        rx.await
            .map_err(|e| internal(format!("writer stopped: {e}")))?
            .map_err(internal)?;
    }
    Ok(Json(json!({"id":id.0})).into_response())
}
async fn logtail_ingest(State(app): State<App>, req: Request) -> Response {
    let run = async {
        let (parts, raw) = req.into_parts();
        let project = app.keys.authenticate(bearer(&parts.headers))?;
        let bytes = body(&parts.headers, raw).await?;
        let content_type = header_str(&parts.headers, header::CONTENT_TYPE).to_owned();
        let records = blocking(move || {
            logtail::parse(&bytes, &content_type, project, time::now_ns())
                .map_err(|e| (StatusCode::NOT_ACCEPTABLE, e))
        })
        .await?;
        if !records.is_empty() {
            queue_logs(&app.logs, records)?;
        }
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
        let bytes = body(&parts.headers, raw).await?;
        let records =
            blocking(move || otlp::parse(&bytes, encoding, project, time::now_ns()).map_err(bad))
                .await?;
        if !records.is_empty() {
            queue_logs(&app.logs, records)?;
        }
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
/// Serves ingest, and `/mcp` when `mcp_token` is set. `public_host` is the host
/// name clients use to reach this server.
pub async fn serve(
    data: &std::path::Path,
    listen: &str,
    public_host: &str,
    log_retention_days: u32,
    mcp_token: Option<Token>,
) -> anyhow::Result<()> {
    let writer = store::Writer::start(store::open(data)?, 1024);
    let log_writer = LogWriter::start(data.join("logs"), log_retention_days)?;
    let app = App {
        keys: Arc::new(Keys::new(store::open(data)?)),
        sender: writer.sender.clone(),
        logs: log_writer.queue.clone(),
    };
    let mut routes = router(app);
    if let Some(token) = mcp_token {
        routes = routes.merge(mcp::router(
            data.to_owned(),
            writer.sender.clone(),
            token,
            public_host,
        ));
    }
    let listener = tokio::net::TcpListener::bind(listen).await?;
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
    use std::io::Write;
    #[test]
    fn compression_formats() {
        let payload = b"{}\n{\"type\":\"event\"}\n{\"message\":\"test\"}";
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gzip.write_all(payload).unwrap();
        let mut deflate =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        deflate.write_all(payload).unwrap();
        let mut br = Vec::new();
        {
            let mut writer = brotli::CompressorWriter::new(&mut br, 4096, 3, 20);
            writer.write_all(payload).unwrap();
        }
        for (encoding, bytes) in [
            ("gzip", gzip.finish().unwrap()),
            ("deflate", deflate.finish().unwrap()),
            ("br", br),
            (
                "zstd",
                zstd::stream::encode_all(payload.as_slice(), 1).unwrap(),
            ),
        ] {
            assert_eq!(decode(&bytes, encoding).unwrap(), payload);
            assert!(decode(b"invalid compressed stream", encoding).is_err());
        }
    }
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
