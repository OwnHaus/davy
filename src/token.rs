//! The admin access token. It guards `/mcp` and the source map upload.

use axum::{
    extract::{Request, State},
    http::{StatusCode, header},
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

/// Middleware that answers `401` unless the request carries `Authorization: Bearer <token>`.
pub async fn require(State(token): State<Arc<Token>>, req: Request, next: Next) -> Response {
    let presented = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    if presented.is_some_and(|p| token.admits(p)) {
        return next.run(req).await;
    }
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Bearer")],
        "missing or wrong access token",
    )
        .into_response()
}
