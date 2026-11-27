//! What Davy's files take on disk, measured when asked by walking the data
//! directory, and the filesystem's free and total space.

mod shared;
#[cfg(unix)]
mod walk;
#[cfg(not(unix))]
#[path = "disk/walk_std.rs"]
mod walk;

pub use shared::Shared;

use super::forecast::NoForecast;
use crate::{logs, store};
use anyhow::{Context as _, Result};
use jiff::civil::Date;
use std::{ffi::OsStr, path::Path, time::Duration};
use walk::{Opened, Walk};

/// The directories `sourcemaps::Store` keeps uploads in.
const UPLOADS: [&str; 2] = ["sourcemaps", "chunks"];
/// Entries one measurement reads at most.
pub const MAX_ENTRIES: u64 = 1_000_000;
/// How long one measurement walks at most.
pub const MAX_TIME: Duration = Duration::from_secs(20);
/// Paths left out that a measurement names. It counts the rest.
const MAX_NAMED: usize = 10;

/// How much one walk reads.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub entries: u64,
    pub time: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            entries: MAX_ENTRIES,
            time: MAX_TIME,
        }
    }
}

/// What a measurement left out. The sizes do not hold it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Gaps {
    /// The first [`MAX_NAMED`] paths left out, under the data directory,
    /// each with why, such as `lost+found: permission denied`.
    pub skipped: Vec<String>,
    /// Paths left out in all.
    pub skipped_count: u64,
    /// Why the walk ended before it read everything.
    pub stopped: Option<String>,
}

impl Gaps {
    pub fn is_empty(&self) -> bool {
        self.skipped_count == 0 && self.stopped.is_none()
    }

    fn skip(&mut self, path: &Path, why: &str) {
        self.skipped_count = self.skipped_count.saturating_add(1);
        if self.skipped.len() < MAX_NAMED {
            self.skipped.push(format!("{}: {why}", path.display()));
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Disk {
    /// Each closed log day's index, oldest first.
    pub closed: Vec<(Date, u64)>,
    /// Each cold log day's files, oldest first.
    pub cold: Vec<(Date, u64)>,
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
    pub gaps: Gaps,
    /// Whether a gap lies under `logs`, or is `logs` itself.
    pub logs_gap: bool,
}

impl Disk {
    /// What logs take, hot and cold.
    pub fn logs(&self) -> u64 {
        self.hot().saturating_add(self.cold_bytes())
    }
    /// What the day indexes take.
    pub fn hot(&self) -> u64 {
        self.closed
            .iter()
            .fold(self.today, |sum, (_, b)| sum.saturating_add(*b))
    }
    /// What the cold days take.
    pub fn cold_bytes(&self) -> u64 {
        self.cold
            .iter()
            .fold(0, |sum, (_, b)| sum.saturating_add(*b))
    }
    /// What Davy's files take.
    pub fn davy(&self) -> u64 {
        self.logs().saturating_add(self.fixed())
    }
    /// What is not logs.
    pub fn fixed(&self) -> u64 {
        self.sqlite
            .saturating_add(self.uploads)
            .saturating_add(self.other)
    }
    /// What the filesystem uses, Davy's files and any others.
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.free)
    }
    /// The share of the filesystem Davy keeps free, see
    /// [`super::forecast::HEADROOM_REASON`].
    pub fn headroom(&self) -> u64 {
        self.total / 100 * super::forecast::HEADROOM_PERCENT
    }
    /// What may still be used before the line of total minus headroom.
    pub fn room_left(&self) -> u64 {
        self.total
            .saturating_sub(self.headroom())
            .saturating_sub(self.used())
    }
    /// Whether the filesystem uses total minus headroom or more already, by
    /// Davy's files or others. The forecast gives no day then once nothing
    /// of Davy's grows, as nothing it does fills the disk.
    pub fn past_limit(&self) -> bool {
        self.room_left() == 0
    }
    /// Why the log days are not all measured, if they are not. A gap
    /// outside `logs` leaves the forecast alone: it takes what is not logs
    /// as constant, and `used` comes from the filesystem.
    pub fn logs_unmeasured(&self) -> Option<NoForecast> {
        if let Some(why) = &self.gaps.stopped {
            return Some(NoForecast::Stopped(format!(
                "the disk walk stopped early, {why}, so not every log day is measured"
            )));
        }
        self.logs_gap.then(|| {
            NoForecast::LogsLeftOut("part of logs is left out of the disk walk (disk.partial says what), so the growth of logs is unknown".to_owned())
        })
    }
}

/// Measures `data`, with `today` the UTC day that is still open. The
/// server measures through [`Shared`], which runs one walk at a time.
pub fn measure(data: &Path, today: Date) -> Result<Disk> {
    measure_within(data, today, Limits::default())
}

/// [`measure`], reading at most what `limits` allow.
pub(crate) fn measure_within(data: &Path, today: Date, limits: Limits) -> Result<Disk> {
    let mut disk = Disk {
        free: fs4::available_space(data)
            .with_context(|| format!("free space of {}", data.display()))?,
        total: fs4::total_space(data).with_context(|| format!("size of {}", data.display()))?,
        ..Disk::default()
    };
    let db = store::DB_FILE;
    let sqlite = [db.to_owned(), format!("{db}-wal"), format!("{db}-shm")];
    let (mut walk, root) = Walk::new(data, limits)?;
    walk.each(&root, |walk, name| {
        if name == logs::DIR {
            return measure_logs(walk, &root, today, &mut disk);
        }
        let bytes = walk.size(&root, name)?;
        let total = if sqlite.iter().any(|s| name == s.as_str()) {
            &mut disk.sqlite
        } else if UPLOADS.iter().any(|u| name == *u) {
            &mut disk.uploads
        } else {
            &mut disk.other
        };
        *total = total.saturating_add(bytes);
        Ok(())
    })?;
    disk.closed.sort_unstable();
    disk.cold.sort_unstable();
    disk.gaps = walk.gaps;
    Ok(disk)
}

/// Sorts the entries of `logs` in `root`: a directory named for a day
/// before `today` is a closed day, one for today or later is today's, and
/// `cold` holds the cold days.
fn measure_logs(walk: &mut Walk, root: &Opened, today: Date, disk: &mut Disk) -> Result<()> {
    let name = OsStr::new(logs::DIR);
    let before = walk.gaps.skipped_count;
    let Some(dir) = walk.open(root, name)? else {
        // Left out by `open`, such as for permission, it is already named.
        if walk.gaps.skipped_count == before {
            disk.other = disk.other.saturating_add(walk.size(root, name)?);
            walk.gaps
                .skip(Path::new(logs::DIR), "a link or not a directory");
        }
        disk.logs_gap = true;
        return Ok(());
    };
    walk.each(&dir, |walk, name| {
        if name == logs::store::cold::DIR && Walk::is_dir(&dir, name) {
            return measure_cold(walk, &dir, disk);
        }
        let bytes = walk.size(&dir, name)?;
        // A day is a directory: a file or link named like one is not.
        let day = name
            .to_str()
            .and_then(logs::store::parse_day)
            .filter(|_| Walk::is_dir(&dir, name));
        let total = match day {
            Some(day) if day < today => {
                disk.closed.push((day, bytes));
                return Ok(());
            }
            Some(_) => &mut disk.today,
            None => &mut disk.other,
        };
        *total = total.saturating_add(bytes);
        Ok(())
    })?;
    disk.logs_gap |= walk.gaps.skipped_count != before;
    Ok(())
}

/// Adds each day directory in `cold` under `logs` to the cold days, and
/// anything else to other. A link named `cold` is no directory to the
/// walk, which follows none, and counts as other. So does a segment still
/// being written, `.N.tmp`, whose logs the hot day holds, and a day with
/// no segment.
fn measure_cold(walk: &mut Walk, logs: &Opened, disk: &mut Disk) -> Result<()> {
    let Some(dir) = walk.open(logs, OsStr::new(logs::store::cold::DIR))? else {
        return Ok(());
    };
    walk.each(&dir, |walk, name| {
        let day = name
            .to_str()
            .and_then(logs::store::parse_day)
            .filter(|_| Walk::is_dir(&dir, name));
        let opened = match day {
            Some(_) => walk.open(&dir, name)?,
            None => None,
        };
        let (Some(day), Some(day_dir)) = (day, opened) else {
            disk.other = disk.other.saturating_add(walk.size(&dir, name)?);
            return Ok(());
        };
        let (mut kept, mut aside) = (0_u64, 0_u64);
        walk.each(&day_dir, |walk, entry| {
            let bytes = walk.size(&day_dir, entry)?;
            if entry.to_str().is_some_and(logs::store::cold::segment_name) {
                kept = kept.saturating_add(bytes);
            } else {
                aside = aside.saturating_add(bytes);
            }
            Ok(())
        })?;
        disk.other = disk.other.saturating_add(aside);
        if kept > 0 {
            disk.cold.push((day, kept));
        }
        Ok(())
    })
}

#[cfg(test)]
#[path = "disk_tests.rs"]
mod tests;
