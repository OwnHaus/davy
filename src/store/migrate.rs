use super::{BUSY_TIMEOUT, MIGRATIONS, init};
use anyhow::{Result, bail};
use rusqlite::{Connection, ErrorCode};
use std::{path::Path, time::Duration};

/// How long a migration waits for other connections to close. SQLite asks the
/// busy handler several times per lock attempt, so `BUSY_TIMEOUT` would make
/// a refusal take about 16 s.
const LOCK_WAIT: Duration = Duration::from_millis(100);

/// Opens `path` with the busy timeout every connection of the store uses.
pub(super) fn connect(path: &Path) -> Result<Connection> {
    let c = Connection::open(path)?;
    c.busy_timeout(BUSY_TIMEOUT)?;
    Ok(c)
}

/// True when the database `c` has not run every migration this build knows.
pub(super) fn pending(c: &Connection) -> Result<bool> {
    let done: u32 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    Ok(usize::try_from(done)? < MIGRATIONS.len())
}

/// Creates the schema and runs the pending migrations on a connection that
/// holds the database file alone, then closes it. A connection of another
/// process in WAL mode holds a shared lock from its first read until it
/// closes, so an older server that still runs makes this fail instead of
/// writing under a schema it does not know.
pub(super) fn alone(path: &Path) -> Result<()> {
    let mut c = Connection::open(path)?;
    c.busy_timeout(LOCK_WAIT)?;
    c.pragma_update(None, "locking_mode", "EXCLUSIVE")?;
    match c.execute_batch("BEGIN EXCLUSIVE; COMMIT;") {
        Err(e) if e.sqlite_error_code() == Some(ErrorCode::DatabaseBusy) => bail!(
            "{} is open in another process, and this Davy must migrate it with no other process using it. Stop the running Davy server and any other davy command, then run this again",
            path.display()
        ),
        locked => locked?,
    }
    init(&mut c)
}

#[cfg(test)]
#[path = "migrate_tests.rs"]
mod tests;
