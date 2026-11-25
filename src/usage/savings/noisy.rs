//! The issues that send most of a project's error events
//! (`docs/design.md`, "Noisy issues"), from `issue_hourly`. Its hours
//! are of the events' own times, so a window counts the events that say
//! they happened in it.

use anyhow::Result;
use rusqlite::{Connection, params};

/// The share of a project's error events an issue sends to be shown, in
/// percent.
pub const MIN_SHARE_PERCENT: u64 = 20;
/// The fewest events an issue sends to be shown: a share of a handful of
/// events is not noise.
pub const MIN_EVENTS: u64 = 100;

#[derive(Debug, PartialEq)]
pub struct Noisy {
    pub id: i64,
    pub project: String,
    pub title: String,
    pub events: u64,
    /// Of the project's error events in the window.
    pub share: f64,
}

/// At most `top` issues of project `id` not discarded that sent
/// [`MIN_SHARE_PERCENT`] or more of its error events, and [`MIN_EVENTS`]
/// or more, in hours from `since_ms` on, the most first.
pub fn noisy(
    c: &Connection,
    (id, project): (i64, &str),
    since_ms: i64,
    top: usize,
) -> Result<Vec<Noisy>> {
    let total: i64 = c.query_row(
        "SELECT COALESCE(SUM(h.count), 0) FROM issue_hourly h JOIN issues i ON i.id=h.issue_id
         WHERE i.project_id=?1 AND h.hour>=?2",
        params![id, since_ms],
        |r| r.get(0),
    )?;
    let total = u64::try_from(total).unwrap_or(0);
    let least = MIN_EVENTS.max((total * MIN_SHARE_PERCENT).div_ceil(100));
    let mut q = c.prepare(
        "SELECT i.id, i.title, SUM(h.count) AS n FROM issues i JOIN issue_hourly h ON h.issue_id=i.id
         WHERE i.project_id=?1 AND h.hour>=?2 AND i.discarded_at IS NULL
         GROUP BY i.id HAVING n>=?3 ORDER BY n DESC, i.id LIMIT ?4",
    )?;
    let rows = q.query_map(
        params![
            id,
            since_ms,
            i64::try_from(least).unwrap_or(i64::MAX),
            i64::try_from(top).unwrap_or(i64::MAX)
        ],
        |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        },
    )?;
    let mut out = Vec::new();
    for row in rows {
        let (issue, title, events) = row?;
        let events = u64::try_from(events).unwrap_or(0);
        out.push(Noisy {
            id: issue,
            project: project.to_owned(),
            title,
            events,
            share: share(events, total),
        });
    }
    Ok(out)
}

/// `part` of `whole`, to two decimals.
#[expect(
    clippy::cast_precision_loss,
    reason = "a share to two decimals does not need the low bits of a count over 2^53"
)]
pub fn share(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        return 0.0;
    }
    (part as f64 / whole as f64 * 100.0).round() / 100.0
}
