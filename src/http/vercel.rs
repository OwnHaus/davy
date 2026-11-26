//! `POST /vercel/logs`, the custom endpoint of a Vercel Log Drain. The drain
//! sends the project key in a custom `Authorization` header, as a Logtail
//! shipper does. A project with a drain secret (`davy project vercel`) also
//! needs `x-vercel-signature`: the hex HMAC-SHA1 of the body under that
//! secret, which Vercel adds to every request.

use super::{
    App, Failure, bad,
    body::{self, Held},
    ingest_logs, limit,
};
use crate::{logs::vercel, time, token::bearer};
use axum::{
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use hmac::{Hmac, KeyInit as _, Mac as _};
use sha1::Sha1;

const SIGNATURE: &str = "x-vercel-signature";
/// Bytes in an HMAC-SHA1.
const SHA1: usize = 20;

/// Vercel's sample endpoint answers `200`, so Davy does too, with no body.
pub(super) async fn ingest(State(app): State<App>, req: Request) -> Response {
    let run = async {
        let (parts, raw) = req.into_parts();
        let project = app.keys.authenticate(bearer(&parts.headers))?;
        let signed = Signed::demand(app.keys.vercel_secret(project)?, &parts.headers)?;
        let outcome = ingest_logs(
            &app,
            project,
            &parts.headers,
            raw,
            app.keys.policy(project)?,
            signed,
            move |body, held| {
                held.take(vercel::cost(body).map_err(bad)?)?;
                vercel::parse(body, project, time::now_ns()).map_err(bad)
            },
        )
        .await;
        let outcome = limit::busy(&app, Some(project), outcome)?;
        Ok::<_, Failure>(outcome.answer(StatusCode::OK, |why| why))
    };
    run.await.unwrap_or_else(IntoResponse::into_response)
}

/// The signature a request to a project with a drain secret carries.
pub(super) struct Signed {
    secret: String,
    signature: [u8; SHA1],
}

impl Signed {
    /// `None` for a project without a secret. For one with a secret, a
    /// missing or malformed header is a `403` before the body is read.
    fn demand(secret: Option<String>, headers: &HeaderMap) -> Result<Option<Self>, Failure> {
        let Some(secret) = secret else {
            return Ok(None);
        };
        let header = headers
            .get(SIGNATURE)
            .ok_or_else(|| refused("missing x-vercel-signature"))?;
        let signature = header
            .to_str()
            .ok()
            .and_then(|h| unhex(h.trim()))
            .ok_or_else(invalid)?;
        Ok(Some(Self { secret, signature }))
    }

    /// Whether the signature is the HMAC of `body`. The comparison takes
    /// the same time wherever the bytes differ.
    fn matches(&self, body: &[u8]) -> bool {
        let Ok(mut mac) = Hmac::<Sha1>::new_from_slice(self.secret.as_bytes()) else {
            return false;
        };
        mac.update(body);
        mac.verify_slice(&self.signature).is_ok()
    }
}

/// The body `bytes` decompressed as `encoding` says, once `signed`, if
/// there is one, matches the bytes as sent. Vercel's sample endpoint signs
/// the raw request body, so the check runs before any decompression, and a
/// body signed only in its decompressed form is refused.
pub(super) fn decode(
    signed: Option<&Signed>,
    bytes: Vec<u8>,
    encoding: &str,
    held: &mut Held,
) -> Result<Vec<u8>, Failure> {
    if let Some(signed) = signed {
        verify(signed, &bytes)?;
    }
    body::decode(bytes, encoding, held)
}

/// A `403` unless `signed` matches the body `bytes` as sent.
pub(super) fn verify(signed: &Signed, bytes: &[u8]) -> Result<(), Failure> {
    if signed.matches(bytes) {
        Ok(())
    } else {
        Err(invalid())
    }
}

fn refused(why: &str) -> Failure {
    (StatusCode::FORBIDDEN, why.to_owned())
}

fn invalid() -> Failure {
    refused("invalid x-vercel-signature")
}

/// 40 hex digits, either case, as 20 bytes.
fn unhex(s: &str) -> Option<[u8; SHA1]> {
    let digits = s.as_bytes();
    if digits.len() != 2 * SHA1 || !digits.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    let mut out = [0; SHA1];
    for (byte, [high, low]) in out.iter_mut().zip(digits.as_chunks::<2>().0) {
        *byte = nibble(*high)? << 4 | nibble(*low)?;
    }
    Some(out)
}

fn nibble(digit: u8) -> Option<u8> {
    char::from(digit)
        .to_digit(16)
        .and_then(|n| u8::try_from(n).ok())
}

#[cfg(test)]
#[path = "vercel_tests.rs"]
mod tests;
