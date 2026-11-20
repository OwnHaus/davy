//! What HTTP checks and alert deliveries share when they reach a URL a
//! user typed.

/// Sent with every check and delivery.
pub const USER_AGENT: &str = concat!("Davy/", env!("CARGO_PKG_VERSION"));
const MAX_ERROR_CHARS: usize = 200;

/// Why a request failed, with its causes, cut to 200 characters. It never
/// holds the URL, because a delivery URL is a credential.
#[must_use]
pub fn error_text(e: reqwest::Error) -> String {
    let e = e.without_url();
    std::iter::successors(Some(&e as &(dyn std::error::Error + 'static)), |e| {
        e.source()
    })
    .map(ToString::to_string)
    .collect::<Vec<_>>()
    .join(": ")
    .chars()
    .take(MAX_ERROR_CHARS)
    .collect()
}
