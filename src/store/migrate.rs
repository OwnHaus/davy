use super::{BUSY_TIMEOUT, SCHEMA_VERSION, init};
use anyhow::{Result, bail};
use rusqlite::{Connection, ErrorCode};
use std::{
    hash::{BuildHasher, RandomState},
    path::Path,
    thread,
    time::{Duration, Instant},
};

/// How long `open` keeps trying to get the database alone for a migration.
/// Another Davy of the same version that opens the store at the same moment
/// holds it for a moment only. An older server holds it until it stops.
pub(super) const WAIT: Duration = Duration::from_secs(10);
/// How long one attempt to get the database alone waits. Two processes that
/// try at once can each hold the shared lock the other waits for, so both
/// give up after this, back off for a random time, and one wins next time.
const LOCK_WAIT: Duration = Duration::from_millis(100);

/// Opens `path` with the busy timeout every connection of the store uses,
/// after running the pending migrations on a connection that holds the file
/// alone. Retries for `wait` while another process has the file open, and
/// stops trying once a peer has migrated.
pub(super) fn open(path: &Path, wait: Duration) -> Result<Connection> {
    let deadline = Instant::now() + wait;
    loop {
        let c = Connection::open(path)?;
        c.busy_timeout(BUSY_TIMEOUT)?;
        match pending(&c) {
            Ok(false) => return Ok(c),
            Ok(true) => drop(c),
            Err(e) if busy(&e) && Instant::now() < deadline => {
                thread::sleep(backoff());
                continue;
            }
            Err(e) if busy(&e) => bail!(
                "{} is locked by another process, most likely a davy that migrates it. Run this again when it is done",
                path.display()
            ),
            Err(e) => return Err(e),
        }
        match alone(path) {
            Ok(()) => {}
            Err(e) if busy(&e) && Instant::now() < deadline => thread::sleep(backoff()),
            Err(e) if busy(&e) => bail!(
                "{} is open in another process, and this Davy must migrate it while no other process has it open. If that process is an older Davy server, stop it or restart it on this version. Otherwise close the program that holds the file, then run this again",
                path.display()
            ),
            Err(e) => return Err(e),
        }
    }
}

/// True when the database `c` has not run every migration this build knows.
fn pending(c: &Connection) -> Result<bool> {
    let done: u32 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    Ok(done < SCHEMA_VERSION)
}

/// Creates the schema and runs the pending migrations on a connection in
/// exclusive locking mode, then closes it. A connection of another process
/// in WAL mode holds a shared lock from its first read until it closes, so
/// an older server that still runs makes this fail instead of writing under
/// a schema it does not know.
fn alone(path: &Path) -> Result<()> {
    let mut c = Connection::open(path)?;
    c.busy_timeout(LOCK_WAIT)?;
    c.pragma_update(None, "locking_mode", "EXCLUSIVE")?;
    // Not IMMEDIATE: on a fresh file, still in rollback mode, EXCLUSIVE takes
    // the lock in one step, so two processes that start together collide
    // less often. See docs/design.md, "Migrations".
    c.execute_batch("BEGIN EXCLUSIVE; COMMIT;")?;
    init(&mut c)
}

fn busy(e: &anyhow::Error) -> bool {
    e.downcast_ref::<rusqlite::Error>()
        .and_then(rusqlite::Error::sqlite_error_code)
        == Some(ErrorCode::DatabaseBusy)
}

/// A pause of 10 to 110 ms that differs between processes and threads.
fn backoff() -> Duration {
    Duration::from_millis(10 + RandomState::new().hash_one(()) % 100)
}

#[cfg(test)]
#[path = "migrate_tests.rs"]
mod tests;
