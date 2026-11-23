//! The Alerts page: the channels and what each was sent, newest first.

use super::{Refusal, issues::Query, issues::Reads, json};
use crate::{alert, event::Timestamp, time::rfc3339_ms};
use anyhow::Result;
use axum::{Router, extract::State, http::Uri, response::Response, routing::get};
use rusqlite::Connection;
use serde::Serialize;
use std::sync::Arc;

/// Notifications on a page.
pub const PAGE: u32 = 50;

pub fn routes(reads: Arc<Reads>) -> Router {
    Router::new()
        .route("/ui/api/channels", get(channels))
        .route("/ui/api/notifications", get(notifications))
        .with_state(reads)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum Delivery {
    /// Not sent yet: waiting for its first try or for the next one.
    Waiting,
    Sent,
    /// Given up on after the last try.
    Failed,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct Notification {
    pub id: i64,
    pub channel: i64,
    /// What it tells, such as `issue.created` or `monitor.down`.
    pub kind: String,
    pub created: String,
    pub delivery: Delivery,
    pub attempts: i64,
    /// When it was sent or given up on.
    pub done: Option<String>,
    /// When the next try is due, while it waits.
    pub next_try: Option<String>,
    /// Why the last try failed. It never holds the channel's URL.
    pub last_error: Option<String>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct NotificationPage {
    pub notifications: Vec<Notification>,
    /// The cursor of the next, older page, when there is one.
    pub next: Option<String>,
}

async fn channels(State(reads): State<Arc<Reads>>, uri: Uri) -> Result<Response, Refusal> {
    Query::parse(&uri, &[])?;
    json(&reads.run(alert::store::list).await?)
}

async fn notifications(State(reads): State<Arc<Reads>>, uri: Uri) -> Result<Response, Refusal> {
    let q = Query::parse(&uri, &["channel", "cursor"])?;
    let number = |key: &str, why: &'static str| {
        q.get(key)
            .map(|v| {
                v.parse::<i64>()
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or(Refusal::bad(why))
            })
            .transpose()
    };
    let channel = number("channel", "channel is the id of a channel")?;
    let before = number("cursor", "cursor is the next value of the page before")?;
    json(&reads.run(move |c| page(c, channel, before)).await?)
}

/// The [`PAGE`] newest notifications of `channel`, or of every channel,
/// with an id below `before`.
pub fn page(c: &Connection, channel: Option<i64>, before: Option<i64>) -> Result<NotificationPage> {
    let mut q = c.prepare(
        "SELECT id,channel_id,type,created_at,attempts,next_at,sent_at,failed_at,last_error
         FROM notifications WHERE (?1 IS NULL OR channel_id=?1) AND (?2 IS NULL OR id<?2)
         ORDER BY id DESC LIMIT ?3",
    )?;
    let mut rows: Vec<Notification> = q
        .query_map(rusqlite::params![channel, before, PAGE + 1], |r| {
            let sent: Option<Timestamp> = r.get(6)?;
            let failed: Option<Timestamp> = r.get(7)?;
            let delivery = match (sent, failed) {
                (Some(_), _) => Delivery::Sent,
                (None, Some(_)) => Delivery::Failed,
                (None, None) => Delivery::Waiting,
            };
            Ok(Notification {
                id: r.get(0)?,
                channel: r.get(1)?,
                kind: r.get(2)?,
                created: rfc3339_ms(r.get(3)?),
                delivery,
                attempts: r.get(4)?,
                done: sent.or(failed).map(rfc3339_ms),
                next_try: (delivery == Delivery::Waiting)
                    .then(|| r.get::<_, Timestamp>(5).map(rfc3339_ms))
                    .transpose()?,
                last_error: r.get(8)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let next = (rows.len() > PAGE as usize).then(|| {
        rows.truncate(PAGE as usize);
        rows.last().map(|n| n.id.to_string())
    });
    Ok(NotificationPage {
        notifications: rows,
        next: next.flatten(),
    })
}
