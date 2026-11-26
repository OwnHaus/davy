//! What ingest answers a project past a daily quota (`docs/design.md`,
//! "Quotas and spike protection"): a `429` with `Retry-After` until
//! midnight UTC, at most [`MAX_WAIT`], and an `X-Sentry-Rate-Limits`
//! entry per refused category, which the [`super::headers`] middleware
//! puts after Davy's fixed [`super::RATE`].

use super::{App, Failure};
use crate::{
    quota::{self, Limits},
    time,
    usage::Signal,
};
use axum::{
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};

/// The rate limit header a handler sets, which the middleware merges.
pub(super) const RATE_LIMITS: &str = "x-sentry-rate-limits";
/// The longest wait Davy asks for, in seconds. A client that obeys a wait
/// until midnight sends nothing after a person raises the quota, so it
/// asks again after 5 min and the gate refuses it again if need be.
pub(super) const MAX_WAIT: u64 = 300;

/// Why ingest refused, or would refuse, some of a request.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Limited {
    /// Seconds until the client may send again.
    retry_after: u64,
    /// `X-Sentry-Rate-Limits` entries, `secs:categories:scope:reason`.
    entries: Vec<String>,
}

impl Limited {
    /// The entries of every signal `limits` refuses, or `None` when it
    /// refuses none.
    pub(super) fn of(limits: &Limits) -> Option<Self> {
        let wait = limits.midnight.min(MAX_WAIT);
        let entries: Vec<String> = quota::CAPPED
            .into_iter()
            .filter(|s| limits.blocks(*s))
            .map(|s| entry(wait, quota::category(s), quota::QUOTA))
            .collect();
        (!entries.is_empty()).then_some(Self {
            retry_after: wait,
            entries,
        })
    }

    /// Sets the rate limit header, and with `retry` `Retry-After`.
    pub(super) fn set(&self, h: &mut HeaderMap, retry: bool) {
        if let Ok(v) = HeaderValue::from_str(&self.entries.join(", ")) {
            h.insert(RATE_LIMITS, v);
        }
        if retry {
            h.insert(header::RETRY_AFTER, HeaderValue::from(self.retry_after));
        }
    }

    /// A `429` with `body`.
    pub(super) fn refuse(&self, body: impl IntoResponse) -> Response {
        let mut response = (StatusCode::TOO_MANY_REQUESTS, body).into_response();
        self.set(response.headers_mut(), true);
        response
    }
}

fn entry(secs: u64, categories: &str, reason: &str) -> String {
    format!("{secs}:{categories}:project:{reason}")
}

/// What `project` may send now.
pub(super) fn limits(app: &App, project: i64) -> Limits {
    app.usage.gate().limits(project, time::now_ns())
}

/// When `limits` refuses `signal`, counts the refused request and says
/// why. For a route that carries one signal, before its body is read: a
/// `/store/` request is one error event, counted as one item, and a log
/// request holds records nobody counted, so it counts as one request.
pub(super) fn refused(app: &App, project: i64, signal: Signal, limits: &Limits) -> Option<Limited> {
    if !limits.blocks(signal) {
        return None;
    }
    let gate = app.usage.gate();
    let now = time::now_ns();
    if signal == Signal::Logs {
        gate.refuse_request(project, quota::QUOTA, now);
    } else {
        gate.refuse(project, quota::QUOTA, quota::category(signal), 1, now);
    }
    Limited::of(limits)
}

/// Counts what an envelope sent of each signal `limits` refused: its
/// first event, and the log records of each skipped log item.
pub(super) fn count_skipped(app: &App, project: i64, skipped: [u64; 2]) {
    let now = time::now_ns();
    for (signal, n) in quota::CAPPED.into_iter().zip(skipped) {
        let category = quota::category(signal);
        app.usage
            .gate()
            .refuse(project, quota::QUOTA, category, n, now);
    }
}

/// Counts a `429` of a full body budget or queue as one busy request of
/// `project`, or of [`quota::NO_PROJECT`] while no key picked one yet. A
/// quota or spike refusal is an answer, not an error, so it is not busy.
pub(super) fn busy<T>(
    app: &App,
    project: Option<i64>,
    result: Result<T, Failure>,
) -> Result<T, Failure> {
    if let Err((StatusCode::TOO_MANY_REQUESTS, _)) = &result {
        let project = project.unwrap_or(quota::NO_PROJECT);
        app.usage
            .gate()
            .refuse_request(project, quota::BUSY, time::now_ns());
    }
    result
}

/// The handler's rate limit entries after Davy's fixed ones, and
/// `Retry-After: 1` on a `429` that did not set its own.
pub(super) fn merge(h: &mut HeaderMap, throttled: bool) {
    let merged = match h.get(RATE_LIMITS).and_then(|v| v.to_str().ok()) {
        Some(own) => HeaderValue::from_str(&format!("{}, {own}", super::RATE)).ok(),
        None => Some(HeaderValue::from_static(super::RATE)),
    };
    if let Some(v) = merged {
        h.insert(RATE_LIMITS, v);
    }
    if throttled {
        h.entry(header::RETRY_AFTER)
            .or_insert(HeaderValue::from_static("1"));
    }
}

#[cfg(test)]
#[path = "limit_tests.rs"]
mod tests;
