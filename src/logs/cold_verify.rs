//! What `davy doctor` reads of the cold tier: the size of each cold day,
//! and every cold file's head, footer and trailer with each block's
//! header and checksum, without decompressing it. Reading the stored
//! bytes once runs at the speed of the disk, while decompressing every
//! row would take minutes on a large tier. A block that fails its check is
//! one a search reports and skips.

use super::{file::ColdFile, layout};
use anyhow::Result;
use jiff::civil::Date;
use std::path::Path;

/// What [`verify`] found.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Verified {
    /// The cold days with the bytes their files take, oldest first.
    pub days: Vec<(Date, u64)>,
    pub files: u64,
    pub blocks: u64,
    /// A line for each file or block that failed its check, the file
    /// first, at most [`MAX_BAD`].
    pub bad: Vec<String>,
    /// Every file or block that failed, also past [`MAX_BAD`].
    pub bad_count: u64,
}

/// The most failures [`Verified::bad`] names.
pub const MAX_BAD: usize = 5;

impl Verified {
    pub fn bytes(&self) -> u64 {
        self.days.iter().map(|(_, b)| b).sum()
    }

    fn fail(&mut self, path: &Path, e: &anyhow::Error) {
        self.bad_count += 1;
        if self.bad.len() < MAX_BAD {
            self.bad.push(format!("{}: {e:#}", path.display()));
        }
    }
}

/// Checks every cold file under the log root `root`. A file or block that
/// fails is counted and named, and the check goes on; a directory that
/// cannot be listed is an error.
pub fn verify(root: &Path) -> Result<Verified> {
    let mut found = Verified::default();
    for day in layout::days(root)? {
        let mut bytes = 0;
        for seq in layout::segments(root, day)? {
            for (project, path) in layout::files(&layout::segment_dir(root, day, seq))? {
                found.files += 1;
                bytes += std::fs::symlink_metadata(&path).map_or(0, |m| m.len());
                if let Err(e) = check(&path, (day, project), &mut found) {
                    found.fail(&path, &e);
                }
            }
        }
        found.days.push((day, bytes));
    }
    Ok(found)
}

/// Checks the file at `path`, of the day and project its path gives, each
/// block that fails noted in `found`.
fn check(path: &Path, (day, project): (Date, u64), found: &mut Verified) -> Result<()> {
    let file = ColdFile::open(path)?;
    file.belongs(day, project)?;
    for i in 0..file.footer.blocks.len() {
        found.blocks += 1;
        if let Err(e) = file.checked(i) {
            found.fail(path, &e);
        }
    }
    Ok(())
}

/// Writes `logs`, all of one day, under the log root `root` and turns
/// their day cold, for tests outside the store. Returns the day.
#[cfg(test)]
pub(crate) fn cold_day(root: &Path, logs: Vec<crate::logs::LogRecord>) -> Result<Date> {
    use super::super::{State, no_overrides};
    let day = crate::time::day_of(logs[0].timestamp);
    let mut state = State::new(root.to_owned(), 30, no_overrides());
    state.add(logs)?;
    state.commit()?;
    state.turn_cold(day)?;
    state.close_all()?;
    Ok(day)
}

#[cfg(test)]
#[path = "cold_verify_tests.rs"]
mod tests;
