//! Channel rows. The tables are created by the migrations in `crate::store`.

use super::{Channel, Destination, Kind, Notice, Secret, retry_after};
use crate::{event::Timestamp, net::MAX_ERROR_CHARS};
use anyhow::{Context as _, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

pub const MAX_CHANNELS: i64 = 100;
/// A channel that is down for a day fills up, and its oldest notices are the
/// ones a team needs, so new ones are counted and dropped.
const MAX_UNSENT: i64 = 100;
/// Feedback has a few places of its own on top of those, so a flood of it
/// cannot push out the alerts.
const MAX_UNSENT_FEEDBACK: i64 = 5;

#[derive(Debug, PartialEq, Eq)]
pub struct Added {
    pub id: i64,
    /// The signing secret of a webhook, shown once.
    pub secret: Option<Secret>,
}

/// Stores a channel for `project`, or for every project when `None`. With
/// `feedback` it also gets what agents report with `submit_feedback`.
pub fn add(
    c: &Connection,
    project: Option<i64>,
    to: &Destination,
    feedback: bool,
    now: Timestamp,
) -> Result<Added> {
    let secret = match to {
        Destination::Webhook(_) => Some(Secret::random()?),
        Destination::Slack(_) | Destination::Email(_) => None,
    };
    let inserted = c.execute(
        "INSERT INTO channels(project_id,kind,url,secret,feedback,created_at)
         SELECT ?1,?2,?3,?4,?5,?6 WHERE (SELECT count(*) FROM channels) < ?7",
        params![
            project,
            to.kind().as_str(),
            to.url(),
            secret.as_ref().map(Secret::as_str),
            feedback,
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
    /// The host of a webhook or Slack URL, because the whole URL of a Slack
    /// webhook is a credential, or the address of an e-mail channel.
    pub to: String,
    pub feedback: bool,
    pub unsent: i64,
    pub failed: i64,
    pub dropped: i64,
}

/// Every channel, oldest first.
pub fn list(c: &Connection) -> Result<Vec<Listed>> {
    let mut q = c.prepare(
        "SELECT ch.id,p.slug,ch.kind,ch.url,ch.feedback,
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
            r.get::<_, bool>(4)?,
            [r.get::<_, i64>(5)?, r.get(6)?, r.get(7)?],
        ))
    })?;
    rows.map(|row| {
        let (id, project, kind, url, feedback, [unsent, failed, dropped]) = row?;
        let kind = Kind::parse(&kind)
            .with_context(|| format!("channel {id} has unknown kind {kind:?}"))?;
        let to = match kind {
            Kind::Email => url.strip_prefix("mailto:").map(str::to_owned),
            Kind::Webhook | Kind::Slack => url::Url::parse(&url)
                .ok()
                .and_then(|u| u.host_str().map(str::to_owned)),
        }
        .with_context(|| format!("channel {id} has no host or address in its URL"))?;
        Ok(Listed {
            id,
            project,
            kind,
            to,
            feedback,
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

/// Queues `notice` for every channel added with `--feedback`, or counts it
/// as dropped when the channel already has [`MAX_UNSENT_FEEDBACK`] unsent.
pub fn notify_feedback(c: &Connection, notice: &Notice, at: Timestamp) -> Result<()> {
    let channels: Vec<(i64, String, i64)> = c
        .prepare(
            "SELECT ch.id,ch.kind,(SELECT count(*) FROM notifications n WHERE n.channel_id=ch.id
               AND n.type='feedback' AND n.sent_at IS NULL AND n.failed_at IS NULL)
             FROM channels ch WHERE ch.feedback=1 ORDER BY ch.id",
        )?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (id, kind, unsent) in channels {
        if unsent >= MAX_UNSENT_FEEDBACK {
            c.execute("UPDATE channels SET dropped=dropped+1 WHERE id=?1", [id])?;
            continue;
        }
        queue(c, id, &kind, None, notice, at)?;
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
/// the channel already has [`MAX_UNSENT`]. Feedback does not count against
/// an alert, so it cannot push one out. The body holds the row's ID, so the
/// row is stored first.
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
        "SELECT count(*) FROM notifications WHERE channel_id=?1 AND sent_at IS NULL AND failed_at IS NULL
           AND (?2='feedback' OR type<>'feedback')",
        params![channel, notice.kind()],
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

/// A notification ready to send.
#[derive(Debug)]
pub struct Due {
    pub id: i64,
    pub channel: i64,
    pub to: Channel,
    pub payload: String,
}

/// The oldest unsent alert of each channel, or its oldest feedback when no
/// alert waits, when it is due by `now`. A row waiting for its retry holds
/// back the newer alerts of its channel, so a receiver never gets
/// `monitor.up` before its `monitor.down`, and feedback never holds back an
/// alert.
pub fn due(c: &Connection, now: Timestamp) -> Result<Vec<Due>> {
    let mut q = c.prepare(
        "SELECT n.id,ch.id,ch.kind,ch.url,ch.secret,n.payload FROM channels ch
         JOIN notifications n ON n.id=(SELECT id FROM notifications
           WHERE channel_id=ch.id AND sent_at IS NULL AND failed_at IS NULL
           ORDER BY type='feedback',id LIMIT 1)
         WHERE n.next_at<=?1 ORDER BY n.id",
    )?;
    let rows = q.query_map([now], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, Option<String>>(4)?,
            r.get::<_, String>(5)?,
        ))
    })?;
    rows.map(|row| {
        let (id, channel, kind, url, secret, payload) = row?;
        let to = match (Kind::parse(&kind), secret) {
            (Some(Kind::Webhook), Some(secret)) => Channel::Webhook {
                url,
                secret: Secret::load(secret),
            },
            (Some(Kind::Slack), _) => Channel::Slack { url },
            (Some(Kind::Email), _) => Channel::Email {
                to: url
                    .strip_prefix("mailto:")
                    .with_context(|| format!("e-mail channel {channel} has no mailto: URL"))?
                    .to_owned(),
            },
            (Some(Kind::Webhook), None) => anyhow::bail!("webhook channel {channel} has no secret"),
            (None, _) => anyhow::bail!("channel {channel} has unknown kind {kind:?}"),
        };
        Ok(Due {
            id,
            channel,
            to,
            payload,
        })
    })
    .collect()
}

/// Records one attempt to send row `id` at `at`. A failure keeps the error,
/// cut to 200 characters, and schedules the next try or marks the row failed.
pub fn delivered(
    c: &Connection,
    id: i64,
    at: Timestamp,
    result: &Result<(), String>,
) -> Result<()> {
    let error = match result {
        Ok(()) => {
            c.execute(
                "UPDATE notifications SET attempts=attempts+1,sent_at=?2 WHERE id=?1",
                params![id, at],
            )?;
            return Ok(());
        }
        Err(e) => e.chars().take(MAX_ERROR_CHARS).collect::<String>(),
    };
    let Some(attempts) = c
        .query_row(
            "SELECT attempts FROM notifications WHERE id=?1",
            [id],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
    else {
        return Ok(());
    };
    let attempts = attempts.saturating_add(1);
    let wait = retry_after(u32::try_from(attempts).unwrap_or(u32::MAX))
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    match wait {
        Some(wait) => c.execute(
            "UPDATE notifications SET attempts=?2,next_at=?3,last_error=?4 WHERE id=?1",
            params![id, attempts, at.saturating_add(wait), error],
        )?,
        None => c.execute(
            "UPDATE notifications SET attempts=?2,failed_at=?3,last_error=?4 WHERE id=?1",
            params![id, attempts, at, error],
        )?,
    };
    Ok(())
}
