//! The source map upload `sentry-cli` speaks: chunk upload and artifact bundle
//! assembly. `http::serve` mounts it behind the admin token.

use crate::{
    http::{Failure, bad, blocking, body::inflate, internal, json_response},
    sourcemaps::{Assembly, ChunkId, MAX_FILE, Store},
    token::{self, Token},
};
use axum::{
    Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Multipart, OriginalUri, State},
    middleware,
    response::Response,
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

const CHUNK_SIZE: usize = 8 * 1024 * 1024;
const CHUNKS_PER_REQUEST: usize = 4;
const MAX_REQUEST: usize = CHUNK_SIZE * CHUNKS_PER_REQUEST;
/// Enough chunks for a bundle of [`MAX_FILE`] bytes.
const MAX_ASSEMBLE_CHUNKS: usize = MAX_FILE.div_ceil(CHUNK_SIZE);

#[derive(Clone)]
struct Upload {
    store: Arc<Store>,
    base: Arc<str>,
}

/// The upload routes. `public_url` is where `sentry-cli` reaches this server.
pub fn router(store: Arc<Store>, token: Token, public_url: &url::Url) -> Router {
    let state = Upload {
        store,
        base: public_url.as_str().trim_end_matches('/').into(),
    };
    Router::new()
        .route(
            "/api/0/organizations/{org}/chunk-upload/",
            get(options).post(chunks),
        )
        .route(
            "/api/0/organizations/{org}/artifactbundle/assemble/",
            post(assemble),
        )
        .with_state(state)
        .layer(DefaultBodyLimit::max(MAX_REQUEST + 1024 * 1024))
        .layer(middleware::from_fn_with_state(
            Arc::new(token),
            token::require,
        ))
}

async fn options(State(upload): State<Upload>, OriginalUri(uri): OriginalUri) -> Response {
    json_response(&json!({
        "url": format!("{}{}", upload.base, uri.path()),
        "chunkSize": CHUNK_SIZE,
        "chunksPerRequest": CHUNKS_PER_REQUEST,
        "maxRequestSize": MAX_REQUEST,
        "maxFileSize": MAX_FILE,
        "concurrency": 1,
        "hashAlgorithm": "sha1",
        "compression": ["gzip"],
        "accept": ["artifact_bundles"],
    }))
}

async fn chunks(State(upload): State<Upload>, mut form: Multipart) -> Result<Response, Failure> {
    let mut parts = Vec::new();
    while let Some(field) = form.next_field().await.map_err(bad)? {
        let gzip = match field.name() {
            Some("file") => false,
            Some("file_gzip") => true,
            _ => continue,
        };
        if parts.len() == CHUNKS_PER_REQUEST {
            return Err(bad(format!(
                "a chunk upload holds at most {CHUNKS_PER_REQUEST} parts"
            )));
        }
        let name =
            ChunkId::try_from(field.file_name().unwrap_or_default().to_owned()).map_err(bad)?;
        parts.push((name, gzip, field.bytes().await.map_err(bad)?));
    }
    blocking(move || {
        for (name, gzip, bytes) in parts {
            let bytes = inflate(&bytes, if gzip { "gzip" } else { "" }, CHUNK_SIZE + 1)?;
            if bytes.len() > CHUNK_SIZE {
                return Err(bad(format!("chunk {name} is larger than 8 MiB")));
            }
            let actual = ChunkId::of(&bytes);
            if actual != name {
                return Err(bad(format!("chunk {name} hashes to {actual}")));
            }
            upload.store.put_chunk(&actual, &bytes).map_err(internal)?;
        }
        Ok(())
    })
    .await?;
    Ok(json_response(&json!({})))
}

#[derive(Deserialize)]
struct AssembleRequest {
    checksum: ChunkId,
    chunks: Vec<ChunkId>,
}

async fn assemble(State(upload): State<Upload>, body: Bytes) -> Result<Response, Failure> {
    let req: AssembleRequest = serde_json::from_slice(&body).map_err(bad)?;
    if req.chunks.len() > MAX_ASSEMBLE_CHUNKS {
        return Err(bad(format!(
            "an assemble request names at most {MAX_ASSEMBLE_CHUNKS} chunks"
        )));
    }
    let outcome = blocking(move || {
        upload
            .store
            .assemble(&req.checksum, &req.chunks)
            .map_err(internal)
    })
    .await?;
    Ok(json_response(&match outcome {
        Assembly::Missing(chunks) => json!({"state": "not_found", "missingChunks": chunks}),
        Assembly::Created => json!({"state": "created", "missingChunks": []}),
        Assembly::Failed(detail) => {
            json!({"state": "error", "missingChunks": [], "detail": detail})
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sourcemaps::DebugId;
    use crate::test_support::FREE;
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode, header},
    };
    use serde_json::Value;
    use tower::ServiceExt;

    const SECRET: &str = "a-test-token-that-is-long-enough!";
    const CHUNK: &str = "2b57aa67728af071f6a2811f5afcac0748434df7";

    fn server() -> (Router, Arc<Store>) {
        let data = crate::test_support::scratch("upload");
        let store = Arc::new(Store::open(&data));
        let url = url::Url::parse("https://davy.example/").unwrap();
        (router(Arc::clone(&store), Token::new(SECRET), &url), store)
    }

    /// The request `sentry-cli` sent as `n` in `e2e/fixtures/sentry-cli-3/`.
    fn captured(n: u8, token: Option<&str>) -> Request<Body> {
        let meta: Value = serde_json::from_slice(&crate::test_support::fixture(&format!(
            "sentry-cli-3/{n:02}.meta.json"
        )))
        .unwrap();
        let mut req = Request::builder()
            .method(meta["method"].as_str().unwrap())
            .uri(meta["url"].as_str().unwrap());
        if let Some(content_type) = meta["headers"]["content-type"].as_str() {
            req = req.header(header::CONTENT_TYPE, content_type);
        }
        if let Some(t) = token {
            req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
        }
        req.body(Body::from(crate::test_support::fixture(&format!(
            "sentry-cli-3/{n:02}.body"
        ))))
        .unwrap()
    }

    async fn send(app: &Router, req: Request<Body>) -> (StatusCode, String) {
        let response = app.clone().oneshot(req).await.unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), 100_000).await.unwrap();
        (status, String::from_utf8(body.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn every_upload_route_needs_the_token() {
        let (app, _) = server();
        for n in 1..=4 {
            for token in [None, Some("wrong")] {
                assert_eq!(
                    send(&app, captured(n, token)).await.0,
                    StatusCode::UNAUTHORIZED
                );
            }
        }
    }

    #[tokio::test]
    async fn options_point_sentry_cli_at_the_public_url() {
        let (app, _) = server();
        let (status, body) = send(&app, captured(1, Some(SECRET))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            serde_json::from_str::<Value>(&body).unwrap(),
            json!({
                "url": "https://davy.example/api/0/organizations/davy/chunk-upload/",
                "chunkSize": 8_388_608,
                "chunksPerRequest": 4,
                "maxRequestSize": 33_554_432,
                "maxFileSize": 268_435_456,
                "concurrency": 1,
                "hashAlgorithm": "sha1",
                "compression": ["gzip"],
                "accept": ["artifact_bundles"],
            })
        );
    }

    #[tokio::test]
    async fn the_captured_upload_stores_the_build_under_its_debug_id() {
        let (app, store) = server();
        assert_eq!(
            send(&app, captured(2, Some(SECRET))).await,
            (
                StatusCode::OK,
                format!(r#"{{"missingChunks":["{CHUNK}"],"state":"not_found"}}"#)
            )
        );
        assert_eq!(
            send(&app, captured(3, Some(SECRET))).await,
            (StatusCode::OK, "{}".to_owned())
        );
        assert_eq!(
            send(&app, captured(4, Some(SECRET))).await,
            (
                StatusCode::OK,
                r#"{"missingChunks":[],"state":"created"}"#.to_owned()
            )
        );
        let id = DebugId::parse("1c85f8bb-fda9-501c-b106-0fdc0d6937a8").unwrap();
        let mut event = json!({
            "debug_meta": {"images": [{"type": "sourcemap", "code_file": "/app.min.js", "debug_id": id.to_string()}]},
            "exception": {"values": [{"stacktrace": {"frames": [{"filename": "/app.min.js", "function": "n", "lineno": 3, "colno": 74}]}}]},
        });
        crate::sourcemaps::symbolicate(&mut event, |id| store.build(id).unwrap(), FREE).unwrap();
        let frame = &event["exception"]["values"][0]["stacktrace"]["frames"][0];
        assert_eq!(
            [&frame["function"], &frame["filename"], &frame["lineno"]],
            [&json!("parseOrder"), &json!("../src/app.js"), &json!(5)]
        );
    }

    #[tokio::test]
    async fn a_chunk_that_does_not_hash_to_its_name_is_refused() {
        let (app, _) = server();
        let mut req = captured(3, Some(SECRET));
        let mut body = to_bytes(std::mem::take(req.body_mut()), 100_000)
            .await
            .unwrap()
            .to_vec();
        let at = body
            .windows(40)
            .position(|w| w == CHUNK.as_bytes())
            .unwrap();
        body[at..at + 40].fill(b'0');
        *req.body_mut() = Body::from(body);
        let (status, body) = send(&app, req).await;
        assert_eq!(
            (status, body.as_str()),
            (
                StatusCode::BAD_REQUEST,
                "chunk 0000000000000000000000000000000000000000 hashes to 2b57aa67728af071f6a2811f5afcac0748434df7"
            )
        );
    }

    /// A chunk upload with one `file` part per chunk, named by its SHA-1.
    fn chunk_upload(chunks: &[Vec<u8>], gzip: bool) -> Request<Body> {
        let mut body = Vec::new();
        for chunk in chunks {
            let (field, bytes) = if gzip {
                let mut encoder =
                    flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
                std::io::Write::write_all(&mut encoder, chunk).unwrap();
                ("file_gzip", encoder.finish().unwrap())
            } else {
                ("file", chunk.clone())
            };
            body.extend(
                format!(
                    "--b\r\ncontent-disposition: form-data; name=\"{field}\"; filename=\"{}\"\r\n\r\n",
                    ChunkId::of(chunk)
                )
                .as_bytes(),
            );
            body.extend(bytes);
            body.extend(b"\r\n");
        }
        body.extend(b"--b--\r\n");
        Request::post("/api/0/organizations/davy/chunk-upload/")
            .header(header::CONTENT_TYPE, "multipart/form-data; boundary=b")
            .header(header::AUTHORIZATION, format!("Bearer {SECRET}"))
            .body(Body::from(body))
            .unwrap()
    }

    #[tokio::test]
    async fn a_chunk_upload_holds_at_most_4_parts() {
        let (app, _) = server();
        let chunks: Vec<Vec<u8>> = (0..5_u8).map(|n| vec![n]).collect();
        assert_eq!(
            send(&app, chunk_upload(&chunks[..4], false)).await,
            (StatusCode::OK, "{}".to_owned())
        );
        assert_eq!(
            send(&app, chunk_upload(&chunks, false)).await,
            (
                StatusCode::BAD_REQUEST,
                "a chunk upload holds at most 4 parts".to_owned()
            )
        );
    }

    #[tokio::test]
    async fn a_chunk_over_8_mib_unpacked_is_refused() {
        let (app, _) = server();
        let chunk = vec![0; CHUNK_SIZE + 1];
        assert_eq!(
            send(&app, chunk_upload(&[chunk], true)).await,
            (
                StatusCode::BAD_REQUEST,
                "chunk 466f11475dc46ef6e9cebe10afb13eebabbb06eb is larger than 8 MiB".to_owned()
            )
        );
    }

    #[tokio::test]
    async fn an_assemble_request_names_at_most_32_chunks() {
        let (app, _) = server();
        let request = |n: usize| {
            Request::post("/api/0/organizations/davy/artifactbundle/assemble/")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, format!("Bearer {SECRET}"))
                .body(Body::from(
                    json!({"checksum": CHUNK, "chunks": vec![CHUNK; n], "projects": ["shop"]})
                        .to_string(),
                ))
                .unwrap()
        };
        assert_eq!(send(&app, request(32)).await.0, StatusCode::OK);
        assert_eq!(
            send(&app, request(33)).await,
            (
                StatusCode::BAD_REQUEST,
                "an assemble request names at most 32 chunks".to_owned()
            )
        );
    }
}
