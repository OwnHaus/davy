//! Keep the history used by the 30-day uptime views, including incidents
//! that started earlier but overlap the window. Open incidents never expire.

use anyhow::Result;
use rusqlite::{Connection, params};
use std::time::{Duration, Instant};

/// How long one sweep may spend deleting, across every class. The sweep
/// runs in the one writer thread every 10 s, and ingest waits while it
/// runs, so 100 ms costs at most 1% of the writer's time and a burst a
/// tenth of a second of queue. The rate it buys is in `docs/design.md`,
/// "Bounds" of "Cron check-ins".
pub const BUDGET: Duration = Duration::from_millis(100);
/// Rows one statement deletes. Small enough that one takes milliseconds, so
/// the budget is overrun by at most one round of batches.
const BATCH: i64 = 1000;
const KEEP_MS: i64 = 30 * 24 * 60 * 60 * 1000;
const KEEP_STARTS_MS: i64 = 60_000;

/// Each class: a statement that deletes up to `?2` rows older than `?1`,
/// and how long its rows are kept.
const CLASSES: [(&str, i64); 5] = [
    // CROSS JOIN keeps monitors as the outer loop, so each monitor's old
    // rows are a range of checks_monitor_at instead of a scan of every check.
    (
        "DELETE FROM checks WHERE rowid IN (SELECT c.rowid FROM monitors m
         CROSS JOIN checks c ON c.monitor_id=m.id AND c.at<?1 LIMIT ?2)",
        KEEP_MS,
    ),
    (
        "DELETE FROM incidents WHERE rowid IN (SELECT rowid FROM incidents WHERE resolved_at<?1 LIMIT ?2)",
        KEEP_MS,
    ),
    (
        "DELETE FROM notifications WHERE rowid IN (SELECT rowid FROM notifications WHERE sent_at<?1 LIMIT ?2)",
        KEEP_MS,
    ),
    (
        "DELETE FROM notifications WHERE rowid IN (SELECT rowid FROM notifications WHERE failed_at<?1 LIMIT ?2)",
        KEEP_MS,
    ),
    (
        "DELETE FROM checkin_starts WHERE rowid IN (SELECT s.rowid FROM monitors m
         CROSS JOIN checkin_starts s ON s.monitor_id=m.id AND s.at<?1 LIMIT ?2)",
        KEEP_STARTS_MS,
    ),
];

/// Deletes the rows that expired by `at` in rounds of one batch per class,
/// until no class has a full batch left or `budget` is spent. The first
/// round always runs, so no class gets less than one batch per sweep, and
/// the rounds take turns, so a class with a backlog cannot use up the
/// budget of the others.
pub fn prune(c: &Connection, at: i64, budget: Duration) -> Result<()> {
    let deadline = Instant::now().checked_add(budget);
    let mut more = [true; CLASSES.len()];
    loop {
        for ((sql, keep), more) in CLASSES.iter().zip(&mut more) {
            if *more {
                let deleted = c.execute(sql, params![at.saturating_sub(*keep), BATCH])?;
                *more = i64::try_from(deleted).is_ok_and(|n| n >= BATCH);
            }
        }
        if !more.contains(&true) || deadline.is_some_and(|d| Instant::now() >= d) {
            return Ok(());
        }
    }
}
