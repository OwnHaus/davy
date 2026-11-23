//! Channel rows. The tables are created by the migrations in `crate::store`.

use super::{
    Channel, Destination, Kind, Notice, Secret,
    digest::{Digest, Entry, Held, WINDOW_MS},
    retry_after,
};
use crate::{event::Timestamp, net::MAX_ERROR_CHARS};
use anyhow::{Context as _, Result};
use rusqlite::{
    Connection, OptionalExtension, params,
    types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef},
};
use serde::Serialize;
use url::Url;

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
            to.kind(),
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
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename = "Channel")]
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
            r.get::<_, Kind>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, bool>(4)?,
            [r.get::<_, i64>(5)?, r.get(6)?, r.get(7)?],
        ))
    })?;
    rows.map(|row| {
        let (id, project, kind, url, feedback, [unsent, failed, dropped]) = row?;
        let to = match kind {
            Kind::Email => url.strip_prefix("mailto:").map(str::to_owned),
            Kind::Webhook | Kind::Slack => Url::parse(&url)
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
    fan_out(c, project, true, notice, at)
}

/// Queues `notice` for the channels of `project` alone, not for those of
/// all projects. Call it in the transaction that makes the change it reports.
pub fn notify_project(c: &Connection, project: i64, notice: &Notice, at: Timestamp) -> Result<()> {
    fan_out(c, project, false, notice, at)
}

fn fan_out(
    c: &Connection,
    project: i64,
    all_projects: bool,
    notice: &Notice,
    at: Timestamp,
) -> Result<()> {
    let channels: Vec<(i64, Kind)> = c
        .prepare(
            "SELECT id,kind FROM channels WHERE project_id=?1 OR (?2 AND project_id IS NULL) ORDER BY id",
        )?
        .query_map(params![project, all_projects], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    if channels.is_empty() {
        return Ok(());
    }
    let slug: String = c.query_row("SELECT slug FROM projects WHERE id=?1", [project], |r| {
        r.get(0)
    })?;
    for (id, kind) in channels {
        queue(c, id, kind, Some(&slug), notice, at)?;
    }
    Ok(())
}

/// Queues `notice` for every channel added with `--feedback`, or counts it
/// as dropped when the channel already has [`MAX_UNSENT_FEEDBACK`] unsent.
pub fn notify_feedback(c: &Connection, notice: &Notice, at: Timestamp) -> Result<()> {
    let channels: Vec<(i64, Kind, i64)> = c
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
        queue(c, id, kind, None, notice, at)?;
    }
    Ok(())
}

/// Queues a test notice for channel `id`. False when there is none.
pub fn test(c: &Connection, id: i64, at: Timestamp) -> Result<bool> {
    let Some((kind, project)) = c
        .query_row(
            "SELECT ch.kind,p.slug FROM channels ch LEFT JOIN projects p ON p.id=ch.project_id WHERE ch.id=?1",
            [id],
            |r| Ok((r.get::<_, Kind>(0)?, r.get::<_, Option<String>>(1)?)),
        )
        .optional()?
    else {
        return Ok(false);
    };
    queue(c, id, kind, project.as_deref(), &Notice::Test, at)?;
    Ok(true)
}

/// The outbox budget a notice of `kind` counts against.
fn lane_class(kind: &str) -> &'static str {
    match kind {
        "monitor.digest" => "sdk",
        "issue.new" | "issue.regressed" => "issue",
        "feedback" => "feedback",
        _ => "operator",
    }
}

/// Stores one notification row, or counts it as dropped when the channel
/// already has [`MAX_UNSENT`] of its class: issue notices, SDK monitor
/// notices and the operator's own monitor and test notices each have their
/// own. Feedback does not count against an alert, so it cannot push one out. The body holds the row's ID, so the row is stored
/// first. An issue notice of a project is due when the channel's waiting
/// issue rows are, or [`WINDOW_MS`] after `at` when none waits, and keeps
/// its issue for a digest. Any other notice is due at once.
fn queue(
    c: &Connection,
    channel: i64,
    kind: Kind,
    project: Option<&str>,
    notice: &Notice,
    at: Timestamp,
) -> Result<()> {
    let sdk = super::monitor_digest::sdk(c, notice)?;
    let lane = if sdk { "monitor.digest" } else { notice.kind() };
    // Each class has its own 100 rows, so a DSN holder who makes new issues
    // or flaps SDK monitors cannot crowd out a CLI or HTTP monitor notice.
    // Feedback counts every row but the SDK ones: it never pushes out alerts.
    let unsent: i64 = c.query_row(
        "SELECT count(*) FROM notifications WHERE channel_id=?1 AND sent_at IS NULL AND failed_at IS NULL
           AND CASE WHEN type='monitor.digest' THEN 'sdk' WHEN type IN ('issue.new','issue.regressed') THEN 'issue'
             WHEN type='feedback' THEN 'feedback' ELSE 'operator' END
           IN (?2, iif(?2='feedback','issue',NULL), iif(?2='feedback','operator',NULL))",
        params![channel, lane_class(lane)],
        |r| r.get(0),
    )?;
    if unsent >= MAX_UNSENT {
        c.execute(
            "UPDATE channels SET dropped=dropped+1 WHERE id=?1",
            [channel],
        )?;
        return Ok(());
    }
    let held = notice
        .issue()
        .zip(project)
        .map(|(issue, project)| {
            serde_json::to_string(&Held {
                project: project.to_owned(),
                issue: issue.clone(),
            })
        })
        .transpose()?;
    c.execute(
        "INSERT INTO notifications(channel_id,type,payload,created_at,next_at,issue)
         VALUES (?1,?2,'',?3,CASE WHEN ?2='monitor.digest' THEN coalesce((SELECT min(next_at) FROM notifications WHERE channel_id=?1 AND type='monitor.digest' AND sent_at IS NULL AND failed_at IS NULL AND attempts=0),?3+?5) WHEN ?4 IS NULL THEN ?3 ELSE coalesce((SELECT max(next_at)
           FROM notifications WHERE channel_id=?1 AND issue IS NOT NULL AND attempts=0
           AND sent_at IS NULL AND failed_at IS NULL AND next_at>?3),?3+?5) END,?4)",
        params![channel, lane, at, held, WINDOW_MS],
    )?;
    let id = c.last_insert_rowid();
    c.execute(
        "UPDATE notifications SET payload=?2 WHERE id=?1",
        params![
            id,
            notice.render(if sdk { Kind::Webhook } else { kind }, id, project, at)?
        ],
    )?;
    Ok(())
}

/// A notification ready to send.
#[derive(Debug)]
pub struct Due {
    pub id: i64,
    /// The other rows of a digest, marked with this one.
    pub merged: Vec<i64>,
    pub channel: i64,
    pub to: Channel,
    pub payload: String,
}

/// The oldest unsent alert of each channel, or its oldest feedback when no
/// alert waits, when it is due by `now`. A row waiting for its retry holds
/// back the newer alerts of its channel, so a receiver never gets
/// `monitor.up` before its `monitor.down`, and feedback never holds back an
/// alert. Issue rows that wait for their window hold back nothing. When
/// their window ends they go out together, as one [`Digest`] when there are
/// several, with links that start at `public_url`.
pub fn due(c: &Connection, now: Timestamp, public_url: &Url) -> Result<Vec<Due>> {
    let mut q = c.prepare(
        "SELECT n.id,ch.id,ch.kind,ch.url,ch.secret,n.payload,n.attempts,n.issue IS NOT NULL,n.type='monitor.digest'
         FROM channels ch JOIN notifications n ON n.id=(SELECT id FROM notifications
           WHERE channel_id=ch.id AND sent_at IS NULL AND failed_at IS NULL
           AND (type<>'monitor.digest' OR (next_at<=?1 AND ch.sdk_next_at<=?1))
           ORDER BY issue IS NOT NULL AND attempts=0 AND next_at>?1,type='monitor.digest',type='feedback',id LIMIT 1)
         WHERE n.next_at<=?1 ORDER BY n.id",
    )?;
    let rows = q.query_map([now], |r| {
        Ok((
            [r.get::<_, i64>(0)?, r.get(1)?, r.get(6)?],
            r.get::<_, Kind>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, Option<String>>(4)?,
            r.get::<_, String>(5)?,
            r.get::<_, bool>(7)?,
            r.get::<_, bool>(8)?,
        ))
    })?;
    rows.map(|row| {
        let ([id, channel, attempts], kind, url, secret, payload, issue, sdk) = row?;
        let mut due = Due {
            id,
            merged: Vec::new(),
            channel,
            to: channel_of(channel, kind, url, secret)?,
            payload,
        };
        if issue && let Some((digest, merged)) = digest(c, channel, attempts, now)? {
            due.payload = digest.render(kind, public_url)?;
            due.merged = merged;
        }
        if sdk {
            super::monitor_digest::render(c, &mut due, kind, now)?;
        }
        Ok(due)
    })
    .collect()
}

fn channel_of(channel: i64, kind: Kind, url: String, secret: Option<String>) -> Result<Channel> {
    Ok(match (kind, secret) {
        (Kind::Webhook, Some(secret)) => Channel::Webhook {
            url,
            secret: Secret::load(secret),
        },
        (Kind::Slack, _) => Channel::Slack { url },
        (Kind::Email, _) => Channel::Email {
            to: url
                .strip_prefix("mailto:")
                .with_context(|| format!("e-mail channel {channel} has no mailto: URL"))?
                .to_owned(),
        },
        (Kind::Webhook, None) => anyhow::bail!("webhook channel {channel} has no secret"),
    })
}

/// The digest of the issue rows of `channel` due with its oldest one, with
/// the IDs of the rows after the first, read in one query so the rows marked
/// sent are the rows the body shows. `None` when the oldest goes out alone.
/// Only one group of a channel has been tried at a time, because only its
/// oldest row is sent, so the rows with the oldest one's `attempts` are the
/// rows of a failed digest, or of windows that have ended.
fn digest(
    c: &Connection,
    channel: i64,
    attempts: i64,
    now: Timestamp,
) -> Result<Option<(Digest, Vec<i64>)>> {
    let mut q = c.prepare(
        "SELECT id,created_at,type,issue FROM notifications WHERE channel_id=?1
         AND issue IS NOT NULL AND sent_at IS NULL AND failed_at IS NULL
         AND attempts=?2 AND next_at<=?3 ORDER BY id",
    )?;
    let rows = q.query_map(params![channel, attempts, now], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, Timestamp>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
        ))
    })?;
    let mut first = None;
    let mut entries = Vec::new();
    let mut merged = Vec::new();
    for row in rows {
        let (id, at, kind, held) = row?;
        if first.is_some() {
            merged.push(id);
        }
        first.get_or_insert((id, at));
        let Held { project, issue } = serde_json::from_str(&held)
            .with_context(|| format!("notification {id} holds no issue"))?;
        entries.push(Entry {
            regressed: kind == "issue.regressed",
            project,
            issue,
        });
    }
    Ok(match first {
        Some((id, at)) if !merged.is_empty() => Some((Digest { id, at, entries }, merged)),
        _ => None,
    })
}

/// Records one attempt to send rows `ids` at `at`, every row of a digest
/// alike. A failure keeps the error, cut to 200 characters, and schedules
/// the next try or marks the row failed.
pub fn delivered(
    c: &Connection,
    ids: &[i64],
    at: Timestamp,
    result: &Result<(), String>,
) -> Result<()> {
    for &id in ids {
        attempted(c, id, at, result)?;
    }
    Ok(())
}

fn attempted(c: &Connection, id: i64, at: Timestamp, result: &Result<(), String>) -> Result<()> {
    super::monitor_digest::attempted(c, id, at)?;
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

impl ToSql for Kind {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(self.as_str().into())
    }
}

impl FromSql for Kind {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        let s = value.as_str()?;
        [Self::Webhook, Self::Slack, Self::Email]
            .into_iter()
            .find(|kind| kind.as_str() == s)
            .ok_or_else(|| FromSqlError::Other(format!("unknown channel kind {s:?}").into()))
    }
}
