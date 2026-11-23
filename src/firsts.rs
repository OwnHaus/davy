//! The first time of each step of the setup page. Some steps leave no other
//! trace, such as the first agent on `/mcp` or the first frame a source map
//! moved. The rest are read from their tables and written here once found,
//! so a step stays done after retention deletes the rows that showed it.

use crate::event::Timestamp;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

/// The steps of the setup page, in its order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum First {
    /// A request to `/mcp` with the admin token.
    Agent,
    Project,
    /// An error event, from any SDK.
    Error,
    /// An event whose frame an uploaded source map moved.
    Symbolicated,
    Log,
    Monitor,
    /// A notification a channel accepted.
    Delivered,
}

impl First {
    pub const ALL: [Self; 7] = [
        Self::Agent,
        Self::Project,
        Self::Error,
        Self::Symbolicated,
        Self::Log,
        Self::Monitor,
        Self::Delivered,
    ];

    /// The steps a server needs before it does its job. The rest are worth
    /// doing.
    #[must_use]
    pub fn required(self) -> bool {
        matches!(
            self,
            Self::Agent | Self::Project | Self::Error | Self::Symbolicated
        )
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Project => "project",
            Self::Error => "error",
            Self::Symbolicated => "symbolicated",
            Self::Log => "log",
            Self::Monitor => "monitor",
            Self::Delivered => "delivered",
        }
    }

    /// The query for the first time the tables show, for a step they show.
    fn derived(self) -> Option<&'static str> {
        match self {
            Self::Project => Some("SELECT min(created_at) FROM projects"),
            Self::Error => Some("SELECT min(first_seen) FROM issues"),
            Self::Monitor => Some("SELECT min(created_at) FROM monitors"),
            Self::Delivered => Some("SELECT min(sent_at) FROM notifications"),
            Self::Agent | Self::Symbolicated | Self::Log => None,
        }
    }
}

/// Records `what` at `at`, unless it was seen before.
pub fn record(c: &Connection, what: First, at: Timestamp) -> Result<()> {
    c.execute(
        "INSERT OR IGNORE INTO firsts(what, at) VALUES (?1, ?2)",
        params![what.as_str(), at],
    )?;
    Ok(())
}

/// When each step was first done, in the order of [`First::ALL`], and
/// whether that time came from the tables rather than from `firsts`, so the
/// caller should record it.
pub fn all(c: &Connection) -> Result<Vec<(First, Option<Timestamp>, bool)>> {
    First::ALL
        .into_iter()
        .map(|what| {
            let kept: Option<Timestamp> = c
                .query_row(
                    "SELECT at FROM firsts WHERE what=?1",
                    [what.as_str()],
                    |r| r.get(0),
                )
                .optional()?;
            if kept.is_some() {
                return Ok((what, kept, false));
            }
            let found = match what.derived() {
                Some(sql) => c.query_row(sql, [], |r| r.get::<_, Option<Timestamp>>(0))?,
                None => None,
            };
            Ok((what, found, found.is_some()))
        })
        .collect()
}
