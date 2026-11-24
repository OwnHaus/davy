//! What Davy's files take on disk, measured when asked by walking the data
//! directory, and the filesystem's free and total space.

#[cfg(unix)]
mod walk;
#[cfg(not(unix))]
#[path = "disk/walk_std.rs"]
mod walk;

use crate::{logs, store};
use anyhow::{Context as _, Result};
use jiff::civil::Date;
use std::{
    path::Path,
    sync::{Mutex, PoisonError},
};
use walk::{Opened, Walk};

/// The directories `sourcemaps::Store` keeps uploads in.
const UPLOADS: [&str; 2] = ["sourcemaps", "chunks"];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Disk {
    /// Each closed log day, oldest first.
    pub closed: Vec<(Date, u64)>,
    /// Today's log day, and any after it.
    pub today: u64,
    /// The database with its WAL and shared memory files.
    pub sqlite: u64,
    /// Source maps and upload chunks.
    pub uploads: u64,
    /// Everything else under the data directory.
    pub other: u64,
    pub free: u64,
    pub total: u64,
}

impl Disk {
    pub fn logs(&self) -> u64 {
        self.closed.iter().map(|(_, b)| b).sum::<u64>() + self.today
    }
    /// What Davy's files take.
    pub fn davy(&self) -> u64 {
        self.logs() + self.fixed()
    }
    /// What is not logs.
    pub fn fixed(&self) -> u64 {
        self.sqlite + self.uploads + self.other
    }
    /// What the filesystem uses, Davy's files and any others.
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.free)
    }
}

/// One measurement at a time per process: each walks the whole data
/// directory, so many at once would only compete for the disk. The others
/// wait their turn.
static MEASURING: Mutex<()> = Mutex::new(());

/// Measures `data`, with `today` the UTC day that is still open.
pub fn measure(data: &Path, today: Date) -> Result<Disk> {
    measure_at_most(data, today, walk::MAX_ENTRIES)
}

/// [`measure`], reading at most `limit` entries.
pub(crate) fn measure_at_most(data: &Path, today: Date, limit: u64) -> Result<Disk> {
    let _one = MEASURING.lock().unwrap_or_else(PoisonError::into_inner);
    let mut disk = Disk {
        free: fs4::available_space(data)
            .with_context(|| format!("free space of {}", data.display()))?,
        total: fs4::total_space(data).with_context(|| format!("size of {}", data.display()))?,
        ..Disk::default()
    };
    let db = store::DB_FILE;
    let sqlite = [db.to_owned(), format!("{db}-wal"), format!("{db}-shm")];
    let (mut walk, root) = Walk::new(data, limit)?;
    for name in walk.names(&root)? {
        if name == logs::DIR {
            if let Some(dir) = walk.open(&root, &name)? {
                measure_logs(&mut walk, &dir, today, &mut disk)?;
            }
            continue;
        }
        let bytes = walk.size(&root, &name)?;
        if sqlite.contains(&name) {
            disk.sqlite += bytes;
        } else if UPLOADS.contains(&name.as_str()) {
            disk.uploads += bytes;
        } else {
            disk.other += bytes;
        }
    }
    disk.closed.sort_unstable();
    Ok(disk)
}

/// Sorts the log directory's entries: a directory named for a day before
/// `today` is a closed day, one for today or later is today's.
fn measure_logs(walk: &mut Walk, dir: &Opened, today: Date, disk: &mut Disk) -> Result<()> {
    for name in walk.names(dir)? {
        let bytes = walk.size(dir, &name)?;
        // A day is a directory: a file or link named like one is not.
        let day = match logs::store::parse_day(&name) {
            Some(day) if walk.open(dir, &name)?.is_some() => Some(day),
            _ => None,
        };
        match day {
            Some(day) if day < today => disk.closed.push((day, bytes)),
            Some(_) => disk.today += bytes,
            None => disk.other += bytes,
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "disk_tests.rs"]
mod tests;
