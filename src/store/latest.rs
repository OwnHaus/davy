//! The payload of each issue's newest event.
//!
//! After its first 50 events an issue keeps the payload of every 100th only,
//! in `events.payload`, so the newest event would usually have none. Its
//! payload goes into `latest_payloads`, one row per issue, which the next
//! event replaces. Kept in the `events` row and cleared by the next event,
//! it left a hole in a page SQLite had already filled and moved past, and a
//! hot issue took as much disk as if it kept every payload. Compressing
//! every event to keep the newest one halved ingest of a busy issue, so the
//! writer waits for the end of its transaction and compresses only the
//! newest event of each issue it touched.
use crate::event::ErrorEvent;
use anyhow::Result;
use rusqlite::{Connection, params};
use std::collections::HashMap;

/// Moves the payloads kept only for being the newest out of `events`. Those
/// start with an empty zstd skippable frame, `5D 2A 4D 18 00 00 00 00`,
/// which an issue's newest event carried at most. The work is one index
/// lookup and at most one payload per issue.
pub(super) const MIGRATION: &str = "CREATE TABLE latest_payloads(issue_id INTEGER PRIMARY KEY REFERENCES issues(id), event INTEGER NOT NULL, payload BLOB NOT NULL);
INSERT INTO latest_payloads(issue_id, event, payload) SELECT e.issue_id, e.id, substr(e.payload, 9) FROM issues i JOIN events e ON e.id=(SELECT max(id) FROM events WHERE issue_id=i.id) WHERE substr(e.payload, 1, 8)=x'5D2A4D1800000000';
UPDATE events SET payload=NULL WHERE id IN (SELECT event FROM latest_payloads);";

/// Whether the event at `n`, counting from 0, keeps its payload for good.
pub fn sampled(n: i64) -> bool {
    n < 50 || (n + 1) % 100 == 0
}

/// The newest event of each issue one transaction wrote, by issue id, when
/// it is outside the sample.
#[derive(Default)]
pub struct Latest<'a>(HashMap<i64, Option<(i64, &'a ErrorEvent)>>);

impl<'a> Latest<'a> {
    /// Notes that `e` was written as row `event` of `issue`, with its
    /// payload when `kept`.
    pub fn after(&mut self, issue: i64, event: i64, e: &'a ErrorEvent, kept: bool) {
        self.0.insert(issue, (!kept).then_some((event, e)));
    }

    /// Keeps the payload of each issue's newest event, or drops the one
    /// kept before when the newest is in the sample. Runs once, before the
    /// transaction commits. A sweep in the same transaction may have
    /// deleted the event, and its issue too, when the clock moved on by the
    /// whole retention between taking the event in and queueing the sweep,
    /// so the row is written only while its event is there.
    pub fn keep(self, c: &Connection) -> Result<()> {
        let mut upsert = c.prepare_cached(
            "INSERT INTO latest_payloads(issue_id, event, payload) SELECT ?1, ?2, ?3
             WHERE EXISTS(SELECT 1 FROM events WHERE id=?2 AND issue_id=?1)
             ON CONFLICT(issue_id) DO UPDATE SET event=excluded.event, payload=excluded.payload",
        )?;
        let mut delete = c.prepare_cached("DELETE FROM latest_payloads WHERE issue_id=?1")?;
        for (issue, newest) in self.0 {
            match newest {
                Some((event, e)) => drop(upsert.execute(params![issue, event, compress(e)?])?),
                None => drop(delete.execute([issue])?),
            }
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

#[cfg(test)]
#[path = "latest_tests.rs"]
mod tests;
