//! What the people using an app wrote about it, through the SDK's feedback
//! widget or `Sentry.captureFeedback`.
//!
//! Current SDKs send a `feedback` item, an event whose
//! `contexts.feedback` holds the message. Older ones send a `user_report`
//! item. Both can name the error event they are about, so `issue_context`
//! shows an agent what users said about an issue.

use crate::{event::EventId, event::Timestamp, text};
use anyhow::Result;
use rusqlite::{Connection, params};
use serde::Deserialize;

const DAY_MS: i64 = 86_400_000;
/// Sentry's widget allows 4096 characters too.
pub const MAX_MESSAGE: usize = 4096;
const MAX_NAME: usize = 128;
const MAX_EMAIL: usize = 256;
const MAX_URL: usize = 2048;
/// Rows kept per project and UTC day. The DSN of a browser app is public,
/// so anyone can send feedback.
pub const MAX_PER_DAY: i64 = 1000;
/// The client report reason a feedback past [`MAX_PER_DAY`] is counted under.
pub const OVER_DAILY_CAP: &str = "davy_daily_cap";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserFeedback {
    /// The error event it is about.
    pub event_id: Option<EventId>,
    pub message: String,
    pub name: Option<String>,
    pub email: Option<String>,
    pub url: Option<String>,
}

#[derive(Deserialize)]
struct FeedbackEvent {
    contexts: Contexts,
}

#[derive(Deserialize)]
struct Contexts {
    feedback: Context,
}

#[derive(Deserialize)]
struct Context {
    message: String,
    contact_email: Option<String>,
    name: Option<String>,
    url: Option<String>,
    associated_event_id: Option<String>,
}

#[derive(Deserialize)]
struct UserReport {
    event_id: Option<String>,
    comments: String,
    name: Option<String>,
    email: Option<String>,
}

/// A `feedback` item. `None` when it holds no message.
pub fn parse_feedback(payload: &[u8]) -> Option<UserFeedback> {
    let c = serde_json::from_slice::<FeedbackEvent>(payload)
        .ok()?
        .contexts
        .feedback;
    build(
        &c.message,
        c.associated_event_id.as_deref(),
        c.name.as_deref(),
        c.contact_email.as_deref(),
        c.url.as_deref(),
    )
}

/// A `user_report` item. `None` when it holds no comment.
pub fn parse_user_report(payload: &[u8]) -> Option<UserFeedback> {
    let r: UserReport = serde_json::from_slice(payload).ok()?;
    build(
        &r.comments,
        r.event_id.as_deref(),
        r.name.as_deref(),
        r.email.as_deref(),
        None,
    )
}

fn build(
    message: &str,
    event_id: Option<&str>,
    name: Option<&str>,
    email: Option<&str>,
    url: Option<&str>,
) -> Option<UserFeedback> {
    let message = clean(message, MAX_MESSAGE)?;
    Some(UserFeedback {
        event_id: event_id.and_then(|id| EventId::parse(id).ok()),
        message,
        name: name.and_then(|s| clean(s, MAX_NAME)),
        email: email.and_then(|s| clean(s, MAX_EMAIL)),
        url: url.and_then(|s| clean(s, MAX_URL)),
    })
}

/// `s` trimmed, without control characters and cut to `max` characters.
/// `None` when nothing is left.
fn clean(s: &str, max: usize) -> Option<String> {
    let s = text::printable(s.trim(), ' ');
    let s: String = s.chars().take(max).collect();
    let s = s.trim_end();
    (!s.is_empty()).then(|| s.to_owned())
}

/// Stores `f` for `project`, received at `at`. Returns false when the
/// project already has [`MAX_PER_DAY`] on that UTC day, and nothing was
/// stored.
pub fn add(c: &Connection, project: i64, at: Timestamp, f: &UserFeedback) -> Result<bool> {
    let day = at.div_euclid(DAY_MS) * DAY_MS;
    let today: i64 = c.query_row(
        "SELECT count(*) FROM (SELECT 1 FROM user_feedback WHERE project_id=?1 AND received_at>=?2 LIMIT ?3)",
        params![project, day, MAX_PER_DAY],
        |r| r.get(0),
    )?;
    if today >= MAX_PER_DAY {
        return Ok(false);
    }
    // The event usually came first, as the SDK sends it before the user
    // answers. Feedback about an event Davy has not stored stays unlinked.
    c.execute(
        "INSERT INTO user_feedback(project_id, received_at, event_id, issue_id, message, name, email, url)
         VALUES (?1, ?2, ?3, (SELECT issue_id FROM events WHERE project_id=?1 AND event_id=?3), ?4, ?5, ?6, ?7)",
        params![
            project,
            at,
            f.event_id.as_ref().map(|e| e.0.as_str()),
            f.message,
            f.name,
            f.email,
            f.url
        ],
    )?;
    Ok(true)
}

/// What users said about one issue, newest first.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct IssueFeedback {
    pub received_at: String,
    pub message: String,
    pub name: Option<String>,
    pub email: Option<String>,
    pub url: Option<String>,
    pub event_id: String,
}

/// The newest `limit` feedback rows about events of `issue`.
pub fn of_issue(c: &Connection, issue: i64, limit: usize) -> Result<Vec<IssueFeedback>> {
    let mut q = c.prepare_cached(
        "SELECT received_at, message, name, email, url, event_id FROM user_feedback
         WHERE issue_id=?1 ORDER BY received_at DESC, id DESC LIMIT ?2",
    )?;
    let rows = q.query_map(params![issue, i64::try_from(limit)?], |r| {
        Ok(IssueFeedback {
            received_at: crate::time::rfc3339_ms(r.get(0)?),
            message: r.get(1)?,
            name: r.get(2)?,
            email: r.get(3)?,
            url: r.get(4)?,
            event_id: r.get(5)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
#[path = "user_feedback_tests.rs"]
mod tests;
