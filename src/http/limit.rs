//! What ingest answers a project past a daily quota or a spike rate
//! (`docs/design.md`, "Quotas and spike protection"): a `429` with
//! `Retry-After` until midnight UTC or until the signal's bucket takes
//! items again, at most [`MAX_WAIT`], and an `X-Sentry-Rate-Limits` entry
//! per refused category, which the [`super::headers`] middleware puts
//! after Davy's fixed [`super::RATE`].

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
/// Why a request past a quota or a spike rate is refused.
const OVER_QUOTA: &str = "over the project's daily quota";
const OVER_SPIKE: &str = "over the project's spike rate";

/// Why ingest refused, or would refuse, some of a request.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Limited {
    /// Seconds until the client may send again.
    retry_after: u64,
    /// `X-Sentry-Rate-Limits` entries, `secs:categories:scope:reason`.
    entries: Vec<String>,
    /// Why, for the body of a `429`.
    message: &'static str,
}

/// How long `limits` refuses `signal` and why: until midnight UTC past a
/// quota, or until its bucket takes items again, at most [`MAX_WAIT`].
fn wait(limits: &Limits, signal: Signal) -> Option<(u64, &'static str)> {
    let secs = if limits.blocks(signal) {
        limits.midnight
    } else {
        limits.spiked(signal)?
    };
    Some((secs.min(MAX_WAIT), limits.reason(signal)))
}

fn message(reason: &str) -> &'static str {
    if reason == quota::QUOTA {
        OVER_QUOTA
    } else {
        OVER_SPIKE
    }
}

impl Limited {
    /// The entries of every signal `limits` refuses, with `Retry-After`
    /// the soonest of their waits, or `None` when it refuses none.
    pub(super) fn of(limits: &Limits) -> Option<Self> {
        let waits: Vec<(Signal, u64, &str)> = quota::CAPPED
            .into_iter()
            .filter_map(|s| wait(limits, s).map(|(secs, reason)| (s, secs, reason)))
            .collect();
        let retry_after = waits.iter().map(|w| w.1).min()?;
        let past_quota = waits.iter().any(|w| w.2 == quota::QUOTA);
        Some(Self {
            retry_after,
            entries: waits
                .iter()
                .map(|(s, secs, reason)| entry(*secs, quota::category(*s), reason))
                .collect(),
            message: if past_quota { OVER_QUOTA } else { OVER_SPIKE },
        })
    }

    /// [`Self::of`] for a request of `signal` alone, which waits and is
    /// told why for that signal, or `None` when `limits` takes it.
    pub(super) fn of_signal(limits: &Limits, signal: Signal) -> Option<Self> {
        let (secs, reason) = wait(limits, signal)?;
        Self::of(limits).map(|l| Self {
            retry_after: secs,
            message: message(reason),
            ..l
        })
    }

    /// [`Self::of`] the quotas alone, for a request ingest took: a spike
    /// rate passed by this request waits seconds, and an SDK told on a
    /// `200` would hold back what the bucket takes again by then.
    pub(super) fn taken(limits: &Limits) -> Option<Self> {
        Self::of(&Limits {
            spike: [None; 2],
            ..*limits
        })
    }

    /// Why ingest refused, for the body of a `429`.
    pub(super) fn message(&self) -> &'static str {
        self.message
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

/// When `limits` refuses `signal`, past a quota or its spike rate, counts
/// the refused request and says why. For a route that carries one
/// signal, before its body is read: a `/store/` request is one error
/// event, counted as one item, and a log request holds records nobody
/// counted, so it counts as one request.
pub(super) fn refused(app: &App, project: i64, signal: Signal, limits: &Limits) -> Option<Limited> {
    let limited = Limited::of_signal(limits, signal)?;
    let gate = app.usage.gate();
    let (reason, now) = (limits.reason(signal), time::now_ns());
    if signal == Signal::Logs {
        gate.refuse_request(project, reason, now);
    } else {
        gate.refuse(project, reason, quota::category(signal), 1, now);
    }
    Some(limited)
}

/// Counts what an envelope sent of each signal `limits` refused, past a
/// quota or a spike rate: its first event, and the log records of each
/// skipped log item.
pub(super) fn count_skipped(app: &App, project: i64, skipped: [u64; 2], limits: &Limits) {
    let now = time::now_ns();
    for (signal, n) in quota::CAPPED.into_iter().zip(skipped) {
        let (reason, category) = (limits.reason(signal), quota::category(signal));
        app.usage.gate().refuse(project, reason, category, n, now);
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
