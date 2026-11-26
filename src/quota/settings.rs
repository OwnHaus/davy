//! A project's quota settings, kept in its row of `projects`.

use anyhow::Result;
use rusqlite::{Connection, params};
use std::collections::HashMap;

/// The percent of a quota at which an alert warns, when none is set.
pub const DEFAULT_WARN: u8 = 80;

/// What a project may send in one UTC day.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    /// Error events a day, `None` for no quota.
    pub errors: Option<u64>,
    /// Bytes of logs a day as received, decompressed, `None` for no quota.
    pub logs: Option<u64>,
    /// The percent of a quota at which an alert warns, 1 to 100.
    pub warn: u8,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            errors: None,
            logs: None,
            warn: DEFAULT_WARN,
        }
    }
}

impl Settings {
    /// The quota of the signal in place `i` of [`super::capped`].
    pub fn cap(&self, i: usize) -> Option<u64> {
        [self.errors, self.logs].get(i).copied().flatten()
    }
    /// Whether ingest has anything to check for the project.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

const COLUMNS: &str = "id, quota_errors, quota_logs, quota_warn";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<(i64, Settings)> {
    let count = |n: Option<i64>| n.and_then(|n| u64::try_from(n).ok());
    Ok((
        r.get(0)?,
        Settings {
            errors: count(r.get(1)?),
            logs: count(r.get(2)?),
            warn: u8::try_from(r.get::<_, i64>(3)?).unwrap_or(DEFAULT_WARN),
        },
    ))
}

/// The settings of every project that has any other than the default.
pub fn all(c: &Connection) -> Result<HashMap<i64, Settings>> {
    let mut q = c.prepare_cached(&format!(
        "SELECT {COLUMNS} FROM projects
         WHERE quota_errors IS NOT NULL OR quota_logs IS NOT NULL OR quota_warn<>80"
    ))?;
    let rows = q.query_map([], row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The settings of `project`.
pub fn of(c: &Connection, project: i64) -> Result<Settings> {
    let mut q = c.prepare_cached(&format!("SELECT {COLUMNS} FROM projects WHERE id=?1"))?;
    Ok(q.query_row([project], row)?.1)
}

/// Replaces the settings of `project`.
pub fn store(c: &Connection, project: i64, s: &Settings) -> Result<()> {
    let narrow = |n: Option<u64>| n.map(|n| i64::try_from(n).unwrap_or(i64::MAX));
    c.execute(
        "UPDATE projects SET quota_errors=?2, quota_logs=?3, quota_warn=?4 WHERE id=?1",
        params![project, narrow(s.errors), narrow(s.logs), s.warn],
    )?;
    Ok(())
}
