use crate::{
    envelope::{Disposition, Envelope, disposition},
    event::{ErrorEvent, EventId},
    store::{self, Ingest},
};
use axum::{
    Json, Router,
    body::to_bytes,
    extract::{Path, Request, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::post,
};
use serde_json::json;
use std::{
    io::{Cursor, Read},
    sync::{Arc, Mutex, mpsc},
};
const COMPRESSED: usize = 20 * 1024 * 1024;
const DECOMPRESSED: usize = 200 * 1024 * 1024;
pub const RATE: &str = "86400:transaction;span;replay;profile;profile_chunk:organization";
#[derive(Clone)]
pub struct App {
    pub auth: Arc<Mutex<rusqlite::Connection>>,
    pub sender: mpsc::SyncSender<Ingest>,
}
pub fn router(app: App) -> Router {
    Router::new()
        .route(
            "/api/{project}/envelope/",
            post(envelope).options(|| async { StatusCode::NO_CONTENT }),
        )
        .route(
            "/api/{project}/store/",
            post(legacy).options(|| async { StatusCode::NO_CONTENT }),
        )
        .with_state(app)
        .layer(middleware::from_fn(headers))
}
async fn headers(req: Request, next: Next) -> Response {
    let mut response = next.run(req).await;
    let h = response.headers_mut();
    h.insert("x-sentry-rate-limits", RATE.parse().unwrap());
    h.insert("access-control-allow-origin", "*".parse().unwrap());
    h.insert(
        "access-control-allow-methods",
        "POST, OPTIONS".parse().unwrap(),
    );
    h.insert(
        "access-control-allow-headers",
        "content-type, x-sentry-auth, sentry-trace, baggage"
            .parse()
            .unwrap(),
    );
    h.insert(
        "access-control-expose-headers",
        "X-Sentry-Rate-Limits, Retry-After".parse().unwrap(),
    );
    response
}
type Failure = (StatusCode, String);
fn bad(e: impl std::fmt::Display) -> Failure {
    (StatusCode::BAD_REQUEST, e.to_string())
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
fn authorize(app: &App, project: i64, key: Option<String>) -> Result<(), Failure> {
    let key = key
        .filter(|s| !s.is_empty())
        .ok_or_else(|| (StatusCode::UNAUTHORIZED, "missing key".into()))?;
    let db = app
        .auth
        .lock()
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "database lock".into()))?;
    match store::authenticate(&db, project, &key) {
        Ok(true) => Ok(()),
        Ok(false) => Err((StatusCode::FORBIDDEN, "invalid project key".into())),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    }
}
async fn ingest(app: App, project: i64, req: Request, legacy: bool) -> Result<Response, Failure> {
    let (parts, body) = req.into_parts();
    let early = key(&parts.headers, parts.uri.query(), None);
    if early.is_some() {
        authorize(&app, project, early.clone())?;
    }
    let bytes = to_bytes(body, COMPRESSED)
        .await
        .map_err(|_| (StatusCode::PAYLOAD_TOO_LARGE, "body too large".into()))?;
    let bytes = decode(
        &bytes,
        parts
            .headers
            .get("content-encoding")
            .and_then(|v| v.to_str().ok())
            .unwrap_or(""),
    )?;
    let mut events = Vec::new();
    let (id, dsn) = if legacy {
        events.push(ErrorEvent::parse(serde_json::from_slice(&bytes).map_err(bad)?).map_err(bad)?);
        (None, None)
    } else {
        let envelope = Envelope::parse(&bytes).map_err(bad)?;
        for item in envelope.items {
            if disposition(&item.kind) == Disposition::StoreError {
                events.push(
                    ErrorEvent::parse(serde_json::from_slice(&item.payload).map_err(bad)?)
                        .map_err(bad)?,
                );
            }
        }
        (envelope.header.event_id, envelope.header.dsn)
    };
    if early.is_none() {
        authorize(&app, project, key(&HeaderMap::new(), None, dsn.as_deref()))?;
    }
    let id = id
        .or_else(|| events.first().map(|e| e.event_id.clone()))
        .unwrap_or_else(EventId::fresh);
    if !events.is_empty() {
        let (done, rx) = tokio::sync::oneshot::channel();
        match app.sender.try_send(Ingest {
            project,
            events,
            done,
        }) {
            Ok(()) => (),
            Err(mpsc::TrySendError::Full(_)) => {
                return Ok((
                    StatusCode::TOO_MANY_REQUESTS,
                    [("retry-after", "1")],
                    "ingest queue full",
                )
                    .into_response());
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                return Err((StatusCode::SERVICE_UNAVAILABLE, "writer stopped".into()));
            }
        }
        rx.await
            .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "writer stopped".into()))?
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    }
    Ok(Json(json!({"id":id.0})).into_response())
}
pub async fn serve(data: &std::path::Path, listen: &str) -> anyhow::Result<()> {
    let writer = store::Writer::start(store::open(data)?, 1024);
    let app = App {
        auth: Arc::new(Mutex::new(store::open(data)?)),
        sender: writer.sender.clone(),
    };
    let listener = tokio::net::TcpListener::bind(listen).await?;
    let result = axum::serve(listener, router(app))
        .with_graceful_shutdown(async {
            #[cfg(unix)]
            {
                let mut term =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                        .unwrap();
                tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} }
            }
            #[cfg(not(unix))]
            {
                let _ = tokio::signal::ctrl_c().await;
            }
        })
        .await;
    drop(writer.sender);
    writer
        .thread
        .join()
        .map_err(|_| anyhow::anyhow!("writer panicked"))?;
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
