//! Keep the history used by the 30-day uptime views, including incidents
//! that started earlier but overlap the window. Open incidents never expire.

use anyhow::Result;
use rusqlite::{Connection, params};

pub fn prune(c: &Connection, at: i64) -> Result<()> {
    let cutoff = at.saturating_sub(30 * 24 * 60 * 60 * 1000);
    for (table, column) in [
        ("incidents", "resolved_at"),
        ("notifications", "sent_at"),
        ("notifications", "failed_at"),
    ] {
        c.execute(
            &format!("DELETE FROM {table} WHERE rowid IN (SELECT rowid FROM {table} WHERE {column}<?1 LIMIT 1000)"),
            [cutoff],
        )?;
    }
    c.execute(
        "DELETE FROM checkin_starts WHERE rowid IN (SELECT s.rowid FROM monitors m
         CROSS JOIN checkin_starts s ON s.monitor_id=m.id AND s.at<?1 LIMIT 1000)",
        params![at.saturating_sub(60_000)],
    )?;
    Ok(())
}
