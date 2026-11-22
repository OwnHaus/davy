//! The built UI, embedded by `build.rs`. A path that names no file gets
//! `index.html`, so the app's router decides what it shows, except under
//! `assets/`, where a missing file is a `404`.

use axum::{
    extract::Path,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};

include!(concat!(env!("OUT_DIR"), "/ui_files.rs"));

/// Shown under `/ui/` by a binary built without the UI.
pub(super) const NO_UI: &str = "<!doctype html>
<html lang=\"en\">
<meta charset=\"utf-8\">
<title>Davy</title>
<h1>This Davy was built without its web UI</h1>
<p>Build the UI with <code>scripts/ui</code>, then build Davy again. Ingest, the CLI and MCP work without it.</p>
</html>
";

fn file(name: &str) -> Option<&'static [u8]> {
    FILES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, bytes)| *bytes)
}

fn content_type(name: &str) -> &'static str {
    match name.rsplit_once('.').map_or("", |(_, ext)| ext) {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

pub async fn page(path: Option<Path<String>>) -> Response {
    let name = path.map(|Path(p)| p).unwrap_or_default();
    if let Some(bytes) = file(&name) {
        // Files under assets/ carry a hash of their content in the name.
        let cache = if name.starts_with("assets/") {
            "public, max-age=31536000, immutable"
        } else {
            "no-store"
        };
        let headers = [
            (header::CONTENT_TYPE, content_type(&name)),
            (header::CACHE_CONTROL, cache),
        ];
        return (headers, bytes).into_response();
    }
    if name.starts_with("assets/") {
        return StatusCode::NOT_FOUND.into_response();
    }
    let html = file("index.html").unwrap_or(NO_UI.as_bytes());
    let headers = [
        (header::CONTENT_TYPE, "text/html; charset=utf-8"),
        (header::CACHE_CONTROL, "no-store"),
    ];
    (headers, html).into_response()
}
