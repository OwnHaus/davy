//! The admin access token. It guards `/mcp` and the source map upload.

use axum::{
    extract::{Request, State},
    http::{HeaderMap, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::sync::Arc;

/// An access token, kept only as its hash.
#[derive(Clone)]
pub struct Token(blake3::Hash);

impl Token {
    pub fn new(secret: &str) -> Self {
        Self(blake3::hash(secret.as_bytes()))
    }

    /// `blake3::Hash` compares in constant time.
    fn admits(&self, presented: &str) -> bool {
        blake3::hash(presented.as_bytes()) == self.0
    }
}

/// The credential in `Authorization: Bearer <credential>`. The scheme is
/// case-insensitive, as RFC 9110 says.
pub fn bearer(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, credential) = value.trim().split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then(|| credential.trim())
        .filter(|c| !c.is_empty())
}

/// Middleware that answers `401` unless the request carries `Authorization: Bearer <token>`.
pub async fn require(State(token): State<Arc<Token>>, req: Request, next: Next) -> Response {
    if bearer(req.headers()).is_some_and(|p| token.admits(p)) {
        return next.run(req).await;
    }
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Bearer")],
        "missing or wrong access token",
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::bearer;
    use axum::http::{HeaderMap, HeaderValue, header};

    #[test]
    fn bearer_reads_the_credential_of_any_case_of_the_scheme() {
        for (value, expected) in [
            ("Bearer abc", Some("abc")),
            ("bearer abc", Some("abc")),
            ("BEARER  abc ", Some("abc")),
            ("Basic abc", None),
            ("Bearer", None),
            ("Bearer   ", None),
            ("abc", None),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(header::AUTHORIZATION, HeaderValue::from_static(value));
            assert_eq!(bearer(&headers), expected, "{value:?}");
        }
        assert_eq!(bearer(&HeaderMap::new()), None);
    }
}
