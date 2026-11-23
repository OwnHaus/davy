//! A separate bounded lane for notices whose monitor was made with a public
//! DSN. Operator alerts always pass it, including while it retries.

use super::{Kind, Mail, digest::WINDOW_MS, notice::slack, store::Due};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension as _, params};
use serde_json::Value;

pub fn sdk(c: &Connection, notice: &super::Notice) -> Result<bool> {
    match notice {
        super::Notice::MonitorDown(i) | super::Notice::MonitorUp(i) => Ok(c
            .query_row(
                "SELECT source='sdk' FROM monitors WHERE id=?1",
                [i.monitor],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(false)),
        _ => Ok(false),
    }
}

pub fn render(c: &Connection, due: &mut Due, kind: Kind, now: i64) -> Result<()> {
    let rows = c
        .prepare(
            "SELECT id,payload FROM notifications WHERE channel_id=?1 AND type='monitor.digest'
         AND sent_at IS NULL AND failed_at IS NULL AND next_at<=?2 ORDER BY id",
        )?
        .query_map(params![due.channel, now], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let entries = rows
        .iter()
        .map(|(_, payload)| serde_json::from_str::<Value>(payload))
        .collect::<serde_json::Result<Vec<_>>>()?;
    due.merged = rows
        .iter()
        .map(|(id, _)| *id)
        .filter(|id| *id != due.id)
        .collect();
    let line = format!("{} SDK monitor changes: see /ui/monitors", entries.len());
    due.payload = match kind {
        Kind::Webhook => serde_json::to_string(&serde_json::json!({
            "id": due.id, "type": "monitor.digest", "notices": entries,
        }))?,
        Kind::Slack => serde_json::to_string(&serde_json::json!({
            "text": slack(&line), "unfurl_links": false, "unfurl_media": false,
        }))?,
        Kind::Email => serde_json::to_string(&Mail {
            subject: line.clone(),
            text: format!("{line}\n"),
            message_id: format!("{}.sdk", due.id),
        })?,
    };
    Ok(())
}

/// Cooldown applies to failed attempts too, so retries cannot amplify sends.
pub fn attempted(c: &Connection, id: i64, at: i64) -> Result<()> {
    c.execute(
        "UPDATE channels SET sdk_next_at=?2 WHERE id=(SELECT channel_id FROM notifications
         WHERE id=?1 AND type='monitor.digest')",
        params![id, at.saturating_add(WINDOW_MS)],
    )?;
    Ok(())
}
