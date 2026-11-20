//! Channel rows. The tables are created by the migrations in `crate::store`.

use super::{Kind, Secret};
use crate::{event::Timestamp, monitor::CheckUrl};
use anyhow::{Context as _, Result};
use rusqlite::{Connection, params};
use serde::Serialize;

pub const MAX_CHANNELS: i64 = 100;

#[derive(Debug, PartialEq, Eq)]
pub struct Added {
    pub id: i64,
    /// The signing secret of a webhook, shown once.
    pub secret: Option<Secret>,
}

/// Stores a channel for `project`, or for every project when `None`.
pub fn add(
    c: &Connection,
    project: Option<i64>,
    kind: Kind,
    url: &CheckUrl,
    now: Timestamp,
) -> Result<Added> {
    let secret = match kind {
        Kind::Webhook => Some(Secret::random()?),
        Kind::Slack => None,
    };
    let inserted = c.execute(
        "INSERT INTO channels(project_id,kind,url,secret,created_at)
         SELECT ?1,?2,?3,?4,?5 WHERE (SELECT count(*) FROM channels) < ?6",
        params![
            project,
            kind.as_str(),
            url.as_str(),
            secret.as_ref().map(Secret::as_str),
            now,
            MAX_CHANNELS
        ],
    )?;
    anyhow::ensure!(
        inserted == 1,
        "a data directory holds at most {MAX_CHANNELS} channels"
    );
    Ok(Added {
        id: c.last_insert_rowid(),
        secret,
    })
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Listed {
    pub id: i64,
    /// `None` for a channel of every project.
    pub project: Option<String>,
    pub kind: Kind,
    /// Only the host, because the whole URL of a Slack webhook is a credential.
    pub host: String,
    pub unsent: i64,
    pub failed: i64,
    pub dropped: i64,
}

/// Every channel, oldest first.
pub fn list(c: &Connection) -> Result<Vec<Listed>> {
    let mut q = c.prepare(
        "SELECT ch.id,p.slug,ch.kind,ch.url,
         (SELECT count(*) FROM notifications n WHERE n.channel_id=ch.id AND n.sent_at IS NULL AND n.failed_at IS NULL),
         (SELECT count(*) FROM notifications n WHERE n.channel_id=ch.id AND n.failed_at IS NOT NULL),
         ch.dropped
         FROM channels ch LEFT JOIN projects p ON p.id=ch.project_id ORDER BY ch.id",
    )?;
    let rows = q.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            [r.get::<_, i64>(4)?, r.get(5)?, r.get(6)?],
        ))
    })?;
    rows.map(|row| {
        let (id, project, kind, url, [unsent, failed, dropped]) = row?;
        let kind = Kind::parse(&kind)
            .with_context(|| format!("channel {id} has unknown kind {kind:?}"))?;
        let host = url::Url::parse(&url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned))
            .with_context(|| format!("channel {id} has no host in its URL"))?;
        Ok(Listed {
            id,
            project,
            kind,
            host,
            unsent,
            failed,
            dropped,
        })
    })
    .collect()
}

/// Deletes a channel with its notifications. False when there is none.
pub fn remove(c: &Connection, id: i64) -> Result<bool> {
    Ok(c.execute("DELETE FROM channels WHERE id=?1", [id])? == 1)
}
