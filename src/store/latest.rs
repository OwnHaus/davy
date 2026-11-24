//! The payload of each issue's newest event.
//!
//! After its first 50 events an issue keeps the payload of every 100th only,
//! so the newest event would usually have none. Compressing every event to
//! keep the newest one halved ingest of a busy issue, so the writer waits for
//! the end of its transaction and compresses only the newest event of each
//! issue it touched.
//!
//! A payload kept only for being the newest starts with [`LATEST`], so the
//! next event of the issue clears exactly that one. The sample cannot be
//! worked out from `event_count` later, because the retention sweep lowers
//! it when events expire, and an event in the sample would lose its payload.
//! Payloads stored before the mark stay, at most one per issue.
use crate::event::ErrorEvent;
use anyhow::Result;
use rusqlite::{Connection, params};
use std::collections::HashMap;

/// Marks a payload kept only because its event is the newest of its issue:
/// a zstd skippable frame with no content, magic `0x184D2A5D`, which every
/// zstd decoder skips.
const LATEST: [u8; 8] = [0x5D, 0x2A, 0x4D, 0x18, 0, 0, 0, 0];

/// Whether the event at `n`, counting from 0, keeps its payload for good.
pub fn sampled(n: i64) -> bool {
    n < 50 || (n + 1) % 100 == 0
}

/// The events of one transaction that are the newest of their issue and
/// outside the sample, by issue id.
#[derive(Default)]
pub struct Latest<'a>(HashMap<i64, (i64, &'a ErrorEvent)>);

impl<'a> Latest<'a> {
    /// Notes that `e`, the event at `n` of `issue`, is about to be written.
    /// The event before it loses the payload it kept only for being the
    /// newest, which [`LATEST`] marks. Returns whether `e` keeps its
    /// payload for good.
    pub fn before(&self, c: &Connection, issue: i64, n: i64) -> Result<bool> {
        if n > 0 && !self.0.contains_key(&issue) {
            c.prepare_cached(
                "UPDATE events SET payload=NULL
                 WHERE id=(SELECT max(id) FROM events WHERE issue_id=?1) AND substr(payload,1,8)=?2",
            )?
            .execute(params![issue, LATEST.as_slice()])?;
        }
        Ok(sampled(n))
    }

    /// Notes that `e` was written as row `event` of `issue`.
    pub fn after(&mut self, issue: i64, event: i64, e: &'a ErrorEvent, kept: bool) {
        if kept {
            self.0.remove(&issue);
        } else {
            self.0.insert(issue, (event, e));
        }
    }

    /// Gives the newest event of each issue its payload. Runs once, before
    /// the transaction commits.
    pub fn keep(self, c: &Connection) -> Result<()> {
        let mut update = c.prepare("UPDATE events SET payload=?2 WHERE id=?1")?;
        for (event, e) in self.0.into_values() {
            let mut payload = LATEST.to_vec();
            payload.extend(compress(e)?);
            update.execute(params![event, payload])?;
        }
        Ok(())
    }
}

pub fn compress(e: &ErrorEvent) -> Result<Vec<u8>> {
    Ok(zstd::stream::encode_all(
        serde_json::to_vec(&e.raw)?.as_slice(),
        1,
    )?)
}
