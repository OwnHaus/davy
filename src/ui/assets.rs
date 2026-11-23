//! The built UI, embedded by `build.rs`. A path that names no file gets
//! `index.html`, so the app's router decides what it shows, except under
//! `assets/`, where a missing file is a `404`. `scripts/ui` puts a Brotli copy
//! `name.br` next to each text file, sent to browsers that accept `br`.

use axum::{
    extract::Path,
    http::{HeaderMap, HeaderValue, StatusCode, header},
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

type Files = &'static [(&'static str, &'static [u8])];

fn file(files: Files, name: &str) -> Option<&'static [u8]> {
    files
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, bytes)| *bytes)
}

/// The bytes of `name` in `files`, as its Brotli copy when there is one and
/// `br` is true, with whether they are that copy.
fn pick(files: Files, name: &str, br: bool) -> Option<(&'static [u8], bool)> {
    let packed = br.then(|| file(files, &format!("{name}.br"))).flatten();
    packed
        .map(|b| (b, true))
        .or_else(|| file(files, name).map(|b| (b, false)))
}

/// Whether `Accept-Encoding` names `br` with a weight above zero.
fn accepts_br(headers: &HeaderMap) -> bool {
    headers
        .get_all(header::ACCEPT_ENCODING)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|coding| {
            let mut parts = coding.split(';').map(str::trim);
            parts.next().is_some_and(|c| c.eq_ignore_ascii_case("br"))
                && parts.all(|p| {
                    p.strip_prefix("q=")
                        .and_then(|q| q.parse::<f32>().ok())
                        .is_none_or(|q| q > 0.0)
                })
        })
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

pub async fn page(path: Option<Path<String>>, headers: HeaderMap) -> Response {
    let name = path.map(|Path(p)| p).unwrap_or_default();
    respond(FILES, &name, accepts_br(&headers))
}

fn respond(files: Files, name: &str, br: bool) -> Response {
    // A brotli copy goes out only as the encoding of its plain file.
    let (name, cache) = if name.strip_suffix(".br").is_none() && file(files, name).is_some() {
        // Files under assets/ carry a hash of their content in the name.
        let cache = if name.starts_with("assets/") {
            "public, max-age=31536000, immutable"
        } else {
            "no-store"
        };
        (name, cache)
    } else if name.starts_with("assets/") {
        return StatusCode::NOT_FOUND.into_response();
    } else {
        ("index.html", "no-store")
    };
    let Some((bytes, packed)) = pick(files, name, br) else {
        let headers = [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ];
        return (headers, NO_UI).into_response();
    };
    let mut response = (
        [
            (header::CONTENT_TYPE, content_type(name)),
            (header::CACHE_CONTROL, cache),
            (header::VARY, "accept-encoding"),
        ],
        bytes,
    )
        .into_response();
    if packed {
        response
            .headers_mut()
            .insert(header::CONTENT_ENCODING, HeaderValue::from_static("br"));
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    const FILES: Files = &[
        ("assets/app-1.js", b"plain js"),
        ("assets/app-1.js.br", b"packed js"),
        ("assets/logo-1.png", b"png"),
        ("index.html", b"page"),
        ("index.html.br", b"packed page"),
    ];

    fn accept(value: &str) -> bool {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ACCEPT_ENCODING,
            HeaderValue::from_str(value).unwrap(),
        );
        accepts_br(&headers)
    }

    async fn sent(name: &str, br: bool) -> (u16, Option<String>, Vec<u8>) {
        let r = respond(FILES, name, br);
        let encoding = r
            .headers()
            .get(header::CONTENT_ENCODING)
            .map(|v| v.to_str().unwrap().to_owned());
        let status = r.status().as_u16();
        let body = to_bytes(r.into_body(), 1 << 20).await.unwrap().to_vec();
        (status, encoding, body)
    }

    #[test]
    fn accept_encoding_names_br_only_with_a_weight_above_zero() {
        assert!(accept("gzip, deflate, br, zstd"));
        assert!(accept("BR;q=0.5"));
        assert!(accept("gzip,br"));
        assert!(!accept("br;q=0"));
        assert!(!accept("br;q=0.0, gzip"));
        assert!(!accept("gzip, brotli"));
        assert!(!accept("identity"));
        assert!(!accepts_br(&HeaderMap::new()));
    }

    #[tokio::test]
    async fn the_brotli_copy_goes_only_to_browsers_that_accept_it() {
        let br = Some("br".to_owned());
        assert_eq!(
            sent("assets/app-1.js", true).await,
            (200, br.clone(), b"packed js".to_vec())
        );
        assert_eq!(
            sent("assets/app-1.js", false).await,
            (200, None, b"plain js".to_vec())
        );
        assert_eq!(
            sent("assets/logo-1.png", true).await,
            (200, None, b"png".to_vec())
        );
        assert_eq!(
            sent("issues/5", true).await,
            (200, br, b"packed page".to_vec())
        );
        assert_eq!(sent("issues/5", false).await, (200, None, b"page".to_vec()));
        assert_eq!(sent("assets/gone.js", true).await.0, 404);
        assert_eq!(sent("assets/app-1.js.br", false).await.0, 404);
        assert_eq!(
            sent("index.html.br", false).await,
            (200, None, b"page".to_vec())
        );
        let r = respond(FILES, "assets/app-1.js", true);
        assert_eq!(r.headers()[header::VARY], "accept-encoding");
        assert_eq!(
            r.headers()[header::CONTENT_TYPE],
            "text/javascript; charset=utf-8"
        );
    }

    #[tokio::test]
    async fn a_build_without_the_ui_says_so() {
        let r = respond(&[], "issues/5", true);
        assert_eq!(r.headers().get(header::CONTENT_ENCODING), None);
        let body = to_bytes(r.into_body(), 1 << 20).await.unwrap();
        assert_eq!(&body[..], NO_UI.as_bytes());
    }
}
