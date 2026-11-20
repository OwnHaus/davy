use crate::{
    alert,
    envelope::{Disposition, Envelope, EnvelopeHeader, disposition},
    event::{ErrorEvent, EventId},
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
    token::Token,
    upload,
};
use axum::{
    Json, Router,
    body::{Bytes, to_bytes},
    extract::{Path, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::json;
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Cursor, Read},
    sync::{Arc, Mutex, RwLock, mpsc},
    time::Duration,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
const COMPRESSED: usize = 20 * 1024 * 1024;
const DECOMPRESSED: usize = 200 * 1024 * 1024;
const MAX_HEADER_LINE: usize = 64 * 1024;
const PARSING_SLOTS: usize = 8;
/// Sentry's limit for one event or log item, so a client that works with Sentry fits.
const MAX_ITEM: usize = 1024 * 1024;
const BODY_TIMEOUT: Duration = Duration::from_secs(30);
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
    /// Bounds how many request bodies are decompressed and parsed at once.
    pub parsing: Arc<Semaphore>,
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
        .route("/api/heartbeat/{token}", get(ping).post(ping))
        .route("/api/heartbeat/{token}/fail", get(fail).post(fail))
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
pub(crate) type Failure = (StatusCode, String);
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
fn decoder<'a>(bytes: &'a [u8], encoding: &str) -> Result<impl BufRead + 'a, Failure> {
    let source = Cursor::new(bytes);
    let reader: Box<dyn Read + 'a> = match encoding {
        "" | "identity" => Box::new(source),
        "gzip" => Box::new(flate2::read::GzDecoder::new(source)),
        "deflate" => Box::new(flate2::read::ZlibDecoder::new(source)),
        "br" => Box::new(brotli::Decompressor::new(source, 4096)),
        "zstd" => Box::new(zstd::stream::read::Decoder::new(source).map_err(bad)?),
        _ => return Err(bad("unsupported content encoding")),
    };
    Ok(BufReader::new(reader.take(DECOMPRESSED as u64 + 1)))
}
fn finish(mut reader: impl Read, out: &mut Vec<u8>) -> Result<(), Failure> {
    reader.read_to_end(out).map_err(bad)?;
    if out.len() > DECOMPRESSED {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            "decompressed body too large".into(),
        ));
    }
    Ok(())
}
pub(crate) fn decode(bytes: &[u8], encoding: &str) -> Result<Vec<u8>, Failure> {
    let mut out = Vec::new();
    finish(decoder(bytes, encoding)?, &mut out)?;
    Ok(out)
}
/// Decompresses the envelope header line, passes it to `admit`, and decompresses
/// the rest only if `admit` succeeds. A refused request costs one line.
fn decode_envelope(
    bytes: &[u8],
    encoding: &str,
    admit: impl FnOnce(&EnvelopeHeader) -> Result<(), Failure>,
) -> Result<Vec<u8>, Failure> {
    let mut reader = decoder(bytes, encoding)?;
    let mut out = Vec::new();
    (&mut reader)
        .take(MAX_HEADER_LINE as u64)
        .read_until(b'\n', &mut out)
        .map_err(bad)?;
    if out.last() != Some(&b'\n') && out.len() == MAX_HEADER_LINE {
        return Err(bad("envelope header line too long"));
    }
    admit(&EnvelopeHeader::parse(&out).map_err(bad)?)?;
    finish(reader, &mut out)?;
    Ok(out)
}
fn header_str(headers: &HeaderMap, name: header::HeaderName) -> &str {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .trim()
}
async fn compressed(body: axum::body::Body) -> Result<Bytes, Failure> {
    tokio::time::timeout(BODY_TIMEOUT, to_bytes(body, COMPRESSED))
        .await
        .map_err(|e| (StatusCode::REQUEST_TIMEOUT, format!("body not read: {e}")))?
        .map_err(|e| {
            (
                StatusCode::PAYLOAD_TOO_LARGE,
                format!("body too large: {e}"),
            )
        })
}
/// Takes one parsing slot. The caller holds it until the body is parsed, so at
/// most `PARSING_SLOTS` decompressed bodies are in memory at once.
fn slot(app: &App) -> Result<OwnedSemaphorePermit, Failure> {
    Arc::clone(&app.parsing).try_acquire_owned().map_err(|e| {
        (
            StatusCode::TOO_MANY_REQUESTS,
            format!("too many bodies in flight: {e}"),
        )
    })
}
fn encoding(headers: &HeaderMap) -> String {
    header_str(headers, header::CONTENT_ENCODING).to_ascii_lowercase()
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
fn parse_sentry(
    bytes: &[u8],
    project: i64,
    legacy: bool,
    maps: &sourcemaps::Store,
) -> Result<Parsed, Failure> {
    let event = |json: &[u8]| {
        let mut value = serde_json::from_slice(json).map_err(bad)?;
        // A stored map that no longer reads leaves frames minified. The event still counts.
        symbolicate(&mut value, |id| maps.build(id).ok().flatten());
        ErrorEvent::parse(value).map_err(bad)
    };
    if legacy {
        return Ok(Parsed {
            id: None,
            event: if bytes.len() > MAX_ITEM {
                None
            } else {
                Some(event(bytes)?)
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
                parsed.event = Some(event(&item.payload)?);
            }
            Disposition::StoreError | Disposition::Drop(_) => {}
            Disposition::StoreLogs => {
                if let Ok(records) = logs::sentry::parse(&item.payload, project, observed_at) {
                    parsed.logs.extend(records);
                }
            }
        }
    }
    Ok(parsed)
}
async fn ingest(app: App, project: i64, req: Request, legacy: bool) -> Result<Response, Failure> {
    let (parts, raw) = req.into_parts();
    let presented = key(&parts.headers, parts.uri.query(), None);
    // Only an envelope can carry its key in the body, in the DSN of its header line.
    let key_in_header = presented.is_none() && !legacy;
    if !key_in_header {
        authorize(&app.keys, project, presented.as_deref())?;
    }
    let bytes = compressed(raw).await?;
    let slot = slot(&app)?;
    let encoding = encoding(&parts.headers);
    let (keys, maps) = (Arc::clone(&app.keys), Arc::clone(&app.sourcemaps));
    let parsed = blocking(move || {
        let _slot = slot;
        let body = if key_in_header {
            decode_envelope(&bytes, &encoding, |header| {
                authorize(
                    &keys,
                    project,
                    key(&HeaderMap::new(), None, header.dsn.as_deref()).as_deref(),
                )
            })?
        } else {
            decode(&bytes, &encoding)?
        };
        parse_sentry(&body, project, legacy, &maps)
    })
    .await?;
    let id = parsed
        .id
        .or_else(|| parsed.event.as_ref().map(|e| e.event_id.clone()))
        .unwrap_or_else(EventId::fresh);
    if !parsed.logs.is_empty() {
        queue_logs(&app.logs, parsed.logs)?;
    }
    if let Some(event) = parsed.event {
        let (done, rx) = tokio::sync::oneshot::channel();
        queue(
            &app.sender,
            Write {
                job: Job::Ingest {
                    project,
                    events: vec![event],
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
    let (done, rx) = tokio::sync::oneshot::channel();
    queue(
        &app.sender,
        Write {
            job: Job::Ping {
                monitor,
                at: time::now_ms(),
                failed,
            },
            done,
        },
    )?;
    rx.await
        .map_err(|e| internal(format!("writer stopped: {e}")))?
        .map_err(internal)?;
    Ok("ok")
}
async fn logtail_ingest(State(app): State<App>, req: Request) -> Response {
    let run = async {
        let (parts, raw) = req.into_parts();
        let project = app.keys.authenticate(bearer(&parts.headers))?;
        let bytes = compressed(raw).await?;
        let slot = slot(&app)?;
        let encoding = encoding(&parts.headers);
        let content_type = header_str(&parts.headers, header::CONTENT_TYPE).to_owned();
        let records = blocking(move || {
            let _slot = slot;
            logtail::parse(
                &decode(&bytes, &encoding)?,
                &content_type,
                project,
                time::now_ns(),
            )
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
        let bytes = compressed(raw).await?;
        let slot = slot(&app)?;
        let content_encoding = self::encoding(&parts.headers);
        let records = blocking(move || {
            let _slot = slot;
            otlp::parse(
                &decode(&bytes, &content_encoding)?,
                encoding,
                project,
                time::now_ns(),
            )
            .map_err(bad)
        })
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
    let log_writer = LogWriter::start(data.join("logs"), log_retention_days)?;
    let app = App {
        keys: Arc::new(Keys::new(store::open(data)?)),
        sender: writer.sender.clone(),
        logs: log_writer.queue.clone(),
        sourcemaps: Arc::new(sourcemaps::Store::open(data)),
        parsing: Arc::new(Semaphore::new(PARSING_SLOTS)),
    };
    let maps = Arc::clone(&app.sourcemaps);
    let mut routes = router(app);
    if let Some(token) = token {
        routes = routes
            .merge(mcp::router(
                data.to_owned(),
                writer.sender.clone(),
                token.clone(),
                public_url.host_str().unwrap_or("localhost"),
            ))
            .merge(upload::router(maps, token, public_url));
    }
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
