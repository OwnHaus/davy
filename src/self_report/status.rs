//! What Davy reported of itself lately, for `project_status` and `davy
//! doctor`: its own warnings and errors of the last 24 hours, the crashes
//! recorded or seen in them, and crash files that wait for `davy serve`.

use super::{crash, own};
use crate::{
    logs::{
        self, LogLevel,
        store::{LogQuery, histogram},
    },
    time,
};
use rusqlite::Connection;
use serde::Serialize;
use std::path::Path;

/// The window looked at, a day in milliseconds.
pub const WINDOW_MS: i64 = 24 * 60 * 60 * 1000;
/// Crash issues listed at most.
pub const MAX_CRASHES: i64 = 5;

/// What `project_status` shows of Davy itself: [`Health`], or why it was
/// not read, which does not fail the rest of the status.
#[derive(Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Davy {
    Read(Health),
    Unread { error: String },
}

impl Davy {
    /// [`health`], or why it was not read.
    pub fn read(c: &Connection, data: &Path, now_ms: i64) -> Self {
        health(c, data, now_ms).map_or_else(
            |e| Self::Unread {
                error: format!("{e:#}"),
            },
            Self::Read,
        )
    }
}

#[derive(Debug, PartialEq, Serialize)]
pub struct Health {
    /// The slug of Davy's own project, None until `davy serve` made it.
    pub project: Option<String>,
    /// Davy's own lines at level warn in the last 24 hours.
    pub warnings_24h: u64,
    /// Davy's own lines at level error or fatal in the last 24 hours.
    pub errors_24h: u64,
    /// The crash issues recorded or seen in the last 24 hours, the latest
    /// first.
    pub crashes_24h: Vec<Crash>,
    /// Crash files `davy serve` has not recorded yet.
    pub crashes_waiting: usize,
}

#[derive(Debug, PartialEq, Serialize)]
pub struct Crash {
    pub issue_id: i64,
    pub title: String,
    /// When the newest crash happened, RFC 3339.
    pub last_seen: String,
    /// When the newest crash was recorded, RFC 3339: at the next start of
    /// `davy serve` for a crash that ended it.
    pub recorded_at: String,
    pub event_count: i64,
}

impl Health {
    /// Nothing reported, and `crashes_waiting` crash files.
    pub fn waiting(crashes_waiting: usize) -> Self {
        Health {
            project: None,
            warnings_24h: 0,
            errors_24h: 0,
            crashes_24h: Vec::new(),
            crashes_waiting,
        }
    }

    /// Whether there is anything to look at.
    pub fn quiet(&self) -> bool {
        self.warnings_24h == 0
            && self.errors_24h == 0
            && self.crashes_24h.is_empty()
            && self.crashes_waiting == 0
    }
}

/// What Davy reported of itself in the 24 hours before `now_ms`. Without
/// Davy's own project nothing was reported.
pub fn health(c: &Connection, data: &Path, now_ms: i64) -> anyhow::Result<Health> {
    let crashes_waiting = crash::pending(&crash::dir(data))?.len();
    let Some(own) = own::find(c)? else {
        return Ok(Health::waiting(crashes_waiting));
    };
    let project = own.id;
    let since = now_ms - WINDOW_MS;
    let count = |level| -> anyhow::Result<u64> {
        let q = LogQuery {
            project: Some(project),
            since: since.saturating_mul(1_000_000),
            until: now_ms.saturating_mul(1_000_000),
            min_level: Some(level),
            text: None,
            pattern: None,
            limit: 0,
            skip: 0,
        };
        Ok(histogram(&logs::dir(data), &q)?.counts.iter().sum())
    };
    let errors_24h = count(LogLevel::Error)?;
    let warnings_24h = count(LogLevel::Warn)?.saturating_sub(errors_24h);
    Ok(Health {
        project: Some(own.slug),
        warnings_24h,
        errors_24h,
        crashes_24h: crashes(c, project, since)?,
        crashes_waiting,
    })
}

/// The crash issues of `project` recorded or seen since `since`. A crash
/// that ended Davy is recorded at its next start, which may be days after
/// the panic, or after a panic whose clock was wrong.
fn crashes(c: &Connection, project: i64, since: i64) -> anyhow::Result<Vec<Crash>> {
    let mut stmt = c.prepare(
        "SELECT i.id, i.title, i.last_seen, i.event_count,
                coalesce(max(e.received_at), i.last_seen) AS recorded
         FROM issues i LEFT JOIN events e ON e.issue_id = i.id
         WHERE i.project_id=?1 AND i.level='fatal'
         GROUP BY i.id
         HAVING max(i.last_seen, recorded) >= ?2
         ORDER BY max(i.last_seen, recorded) DESC LIMIT ?3",
    )?;
    let rows = stmt.query_map(rusqlite::params![project, since, MAX_CRASHES], |r| {
        Ok(Crash {
            issue_id: r.get(0)?,
            title: r.get(1)?,
            last_seen: time::rfc3339_ms(r.get(2)?),
            event_count: r.get(3)?,
            recorded_at: time::rfc3339_ms(r.get(4)?),
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

#[cfg(test)]
#[path = "status_tests.rs"]
mod tests;
