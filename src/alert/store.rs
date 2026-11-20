//! Channel rows. The tables are created by the migrations in `crate::store`.

use super::{Kind, Notice, Secret};
use crate::{event::Timestamp, monitor::CheckUrl};
use anyhow::{Context as _, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

pub const MAX_CHANNELS: i64 = 100;
/// A channel that is down for a day fills up, and its oldest notices are the
/// ones a team needs, so new ones are counted and dropped.
const MAX_UNSENT: i64 = 100;

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

/// Queues `notice` for every channel of `project` and of all projects. Call
/// it in the transaction that makes the change it reports.
pub fn notify(c: &Connection, project: i64, notice: &Notice, at: Timestamp) -> Result<()> {
    let channels: Vec<(i64, String)> = c
        .prepare(
            "SELECT id,kind FROM channels WHERE project_id=?1 OR project_id IS NULL ORDER BY id",
        )?
        .query_map([project], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    if channels.is_empty() {
        return Ok(());
    }
    let slug: String = c.query_row("SELECT slug FROM projects WHERE id=?1", [project], |r| {
        r.get(0)
    })?;
    for (id, kind) in channels {
        queue(c, id, &kind, Some(&slug), notice, at)?;
    }
    Ok(())
}

/// Queues a test notice for channel `id`. False when there is none.
pub fn test(c: &Connection, id: i64, at: Timestamp) -> Result<bool> {
    let Some((kind, project)) = c
        .query_row(
            "SELECT ch.kind,p.slug FROM channels ch LEFT JOIN projects p ON p.id=ch.project_id WHERE ch.id=?1",
            [id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
        )
        .optional()?
    else {
        return Ok(false);
    };
    queue(c, id, &kind, project.as_deref(), &Notice::Test, at)?;
    Ok(true)
}

/// Stores one notification row, due at once, or counts it as dropped when
/// the channel already has [`MAX_UNSENT`]. The body holds the row's ID, so
/// the row is stored first.
fn queue(
    c: &Connection,
    channel: i64,
    kind: &str,
    project: Option<&str>,
    notice: &Notice,
    at: Timestamp,
) -> Result<()> {
    let kind = Kind::parse(kind)
        .with_context(|| format!("channel {channel} has unknown kind {kind:?}"))?;
    let unsent: i64 = c.query_row(
        "SELECT count(*) FROM notifications WHERE channel_id=?1 AND sent_at IS NULL AND failed_at IS NULL",
        [channel],
        |r| r.get(0),
    )?;
    if unsent >= MAX_UNSENT {
        c.execute(
            "UPDATE channels SET dropped=dropped+1 WHERE id=?1",
            [channel],
        )?;
        return Ok(());
    }
    c.execute(
        "INSERT INTO notifications(channel_id,type,payload,created_at,next_at) VALUES (?1,?2,'',?3,?3)",
        params![channel, notice.kind(), at],
    )?;
    let id = c.last_insert_rowid();
    c.execute(
        "UPDATE notifications SET payload=?2 WHERE id=?1",
        params![id, notice.render(kind, id, project, at)?],
    )?;
    Ok(())
}
