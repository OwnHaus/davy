//! A project's quota and spike settings, kept in its row of `projects`.

use anyhow::Result;
use rusqlite::{Connection, params};
use std::collections::HashMap;

/// The percent of a quota at which an alert warns, when none is set.
pub const DEFAULT_WARN: u8 = 80;

/// Error events a second a project may send when its spike rate is the
/// default. Log records have no default rate.
pub const DEFAULT_SPIKE: u64 = 4_000;

/// A project's spike rate of one signal, in the column `spike` or
/// `spike_logs`: `NULL` for the default, `0` for off, or the rate.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Spike {
    #[default]
    Default,
    Off,
    /// Items a second, over 0.
    Rate(u64),
}

impl Spike {
    /// Items a second the project may send, `None` when nothing limits
    /// it, with `default` the rate of [`Self::Default`].
    pub fn rate(self, default: Option<u64>) -> Option<u64> {
        match self {
            Self::Default => default,
            Self::Off => None,
            Self::Rate(n) => Some(n),
        }
    }
    /// The setting's name in status JSON.
    pub fn name(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Off => "off",
            Self::Rate(_) => "set",
        }
    }
    fn column(self) -> Option<i64> {
        match self {
            Self::Default => None,
            Self::Off => Some(0),
            Self::Rate(n) => Some(i64::try_from(n).unwrap_or(i64::MAX)),
        }
    }
    fn of_column(n: Option<i64>) -> Self {
        match n.map(u64::try_from) {
            None | Some(Err(_)) => Self::Default,
            Some(Ok(0)) => Self::Off,
            Some(Ok(n)) => Self::Rate(n),
        }
    }
}

/// What a project may send in one UTC day, and in one second.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    /// Error events a day, `None` for no quota.
    pub errors: Option<u64>,
    /// Bytes of logs a day as received, decompressed, `None` for no quota.
    pub logs: Option<u64>,
    /// The percent of a quota at which an alert warns, 1 to 100.
    pub warn: u8,
    /// Error events a second, by default [`DEFAULT_SPIKE`].
    pub spike: Spike,
    /// Log records a second, by default off.
    pub spike_logs: Spike,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            errors: None,
            logs: None,
            warn: DEFAULT_WARN,
            spike: Spike::Default,
            spike_logs: Spike::Default,
        }
    }
}

impl Settings {
    /// The quota of the signal in place `i` of [`super::capped`].
    pub fn cap(&self, i: usize) -> Option<u64> {
        [self.errors, self.logs].get(i).copied().flatten()
    }
    /// The spike rate of the signal in place `i` of [`super::capped`]:
    /// error events or log records a second, `None` for none.
    pub fn rate(&self, i: usize) -> Option<u64> {
        match i {
            0 => self.spike.rate(Some(DEFAULT_SPIKE)),
            _ => self.spike_logs.rate(None),
        }
    }
    /// Whether the project has a daily quota.
    pub fn has_quota(&self) -> bool {
        self.errors.is_some() || self.logs.is_some()
    }
}

const COLUMNS: &str = "id, quota_errors, quota_logs, quota_warn, spike, spike_logs";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<(i64, Settings)> {
    let count = |n: Option<i64>| n.and_then(|n| u64::try_from(n).ok());
    Ok((
        r.get(0)?,
        Settings {
            errors: count(r.get(1)?),
            logs: count(r.get(2)?),
            warn: u8::try_from(r.get::<_, i64>(3)?).unwrap_or(DEFAULT_WARN),
            spike: Spike::of_column(r.get(4)?),
            spike_logs: Spike::of_column(r.get(5)?),
        },
    ))
}

/// The settings of every project that has any other than the default.
pub fn all(c: &Connection) -> Result<HashMap<i64, Settings>> {
    let mut q = c.prepare_cached(&format!(
        "SELECT {COLUMNS} FROM projects
         WHERE quota_errors IS NOT NULL OR quota_logs IS NOT NULL OR quota_warn<>80
            OR spike IS NOT NULL OR spike_logs IS NOT NULL"
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
        "UPDATE projects SET quota_errors=?2, quota_logs=?3, quota_warn=?4, spike=?5,
         spike_logs=?6 WHERE id=?1",
        params![
            project,
            narrow(s.errors),
            narrow(s.logs),
            s.warn,
            s.spike.column(),
            s.spike_logs.column()
        ],
    )?;
    Ok(())
}
