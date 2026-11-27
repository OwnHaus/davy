//! Where the cold tier lies in the log root.
//!
//! ```text
//! logs/cold/YYYY-MM-DD/N/              segment N of the day, from 1 up
//!     <project>.dlc                    a project's logs, see `format`
//!     patterns-<project>.json          its pattern counters
//! logs/cold/YYYY-MM-DD/.N.tmp/         segment N while it is written
//! logs/YYYY-MM-DD/cold-seal            N, while the hot day turns into N
//! ```
//!
//! A day is read as its hot index and its segments, except that a hot day
//! sealed with N is not read once segment N exists: that segment holds the
//! same logs. See "Cold tier" in docs/design.md for the order of the steps
//! and why a crash at any of them loses and doubles nothing.
//!
//! Only real directories and regular files are read, as in the hot root: a
//! symlink is never followed, so nothing outside the data directory is
//! read, renamed or deleted.

use super::super::{day_dirs::real_dir, parse_day};
use anyhow::{Context, Result};
use jiff::civil::Date;
use std::{
    fs::File,
    io::{ErrorKind, Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

/// The directory of the cold tier in the log root.
pub const DIR: &str = "cold";
/// The file in a hot day that names the segment it is turning into.
pub(in crate::logs::store) const SEAL: &str = "cold-seal";
/// The extension of a cold file.
pub(in crate::logs::store) const EXT: &str = "dlc";

pub(in crate::logs::store) fn cold_root(root: &Path) -> PathBuf {
    root.join(DIR)
}

pub(in crate::logs::store) fn day_dir(root: &Path, day: Date) -> PathBuf {
    cold_root(root).join(day.to_string())
}

pub(in crate::logs::store) fn segment_dir(root: &Path, day: Date, seq: u32) -> PathBuf {
    day_dir(root, day).join(seq.to_string())
}

pub(in crate::logs::store) fn temp_dir(root: &Path, day: Date, seq: u32) -> PathBuf {
    day_dir(root, day).join(format!(".{seq}.tmp"))
}

/// The path of `project`'s cold file in the segment at `dir`.
pub(in crate::logs::store) fn file(dir: &Path, project: u64) -> PathBuf {
    dir.join(format!("{project}.{EXT}"))
}

/// The number a segment directory is named by: decimal, from 1, written
/// one way only.
pub(super) fn seq_of(name: &str) -> Option<u32> {
    let n: u32 = name.parse().ok()?;
    (n > 0 && n.to_string() == name).then_some(n)
}

/// The number of a segment being written, from its name `.N.tmp`.
pub(in crate::logs::store) fn temp_of(name: &str) -> Option<u32> {
    seq_of(name.strip_prefix('.')?.strip_suffix(".tmp")?)
}

/// The project of a cold file, from its name `<project>.dlc`.
pub(in crate::logs::store) fn project_of(name: &str) -> Option<u64> {
    let p: u64 = name.strip_suffix(".dlc")?.parse().ok()?;
    (p.to_string() == name[..name.len() - 4]).then_some(p)
}

/// The names in `dir` with the type of each entry, not of a symlink's
/// target. Empty when `dir` does not exist; an error when it is not a
/// real directory.
fn entries(dir: &Path) -> Result<Vec<(String, std::fs::FileType)>> {
    if !real_dir(dir)? {
        return Ok(Vec::new());
    }
    let read = match std::fs::read_dir(dir) {
        Ok(read) => read,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("listing {}", dir.display())),
    };
    let mut out = Vec::new();
    for e in read {
        let e = e?;
        out.push((e.file_name().to_string_lossy().into_owned(), e.file_type()?));
    }
    Ok(out)
}

/// The days that have a cold directory, oldest first.
pub(in crate::logs::store) fn days(root: &Path) -> Result<Vec<Date>> {
    let mut days: Vec<Date> = entries(&cold_root(root))?
        .into_iter()
        .filter(|(_, t)| t.is_dir())
        .filter_map(|(name, _)| parse_day(&name))
        .collect();
    days.sort_unstable();
    Ok(days)
}

/// The segments of `day`, lowest first.
pub(in crate::logs::store) fn segments(root: &Path, day: Date) -> Result<Vec<u32>> {
    let mut seqs: Vec<u32> = entries(&day_dir(root, day))?
        .into_iter()
        .filter(|(_, t)| t.is_dir())
        .filter_map(|(name, _)| seq_of(&name))
        .collect();
    seqs.sort_unstable();
    Ok(seqs)
}

/// The segments of `day` being written, by number.
pub(in crate::logs::store) fn temps(root: &Path, day: Date) -> Result<Vec<u32>> {
    Ok(entries(&day_dir(root, day))?
        .into_iter()
        .filter(|(_, t)| t.is_dir())
        .filter_map(|(name, _)| temp_of(&name))
        .collect())
}

/// The cold files of the segment at `dir`, by project.
pub(in crate::logs::store) fn files(dir: &Path) -> Result<Vec<(u64, PathBuf)>> {
    let mut files: Vec<(u64, PathBuf)> = entries(dir)?
        .into_iter()
        .filter(|(_, t)| t.is_file())
        .filter_map(|(name, _)| project_of(&name).map(|p| (p, dir.join(name))))
        .collect();
    files.sort_unstable();
    Ok(files)
}

/// The segment the hot day at `hot` is sealed for, if it is. A seal that
/// cannot be read as one is none: only a crash while it was written leaves
/// one, and the start after it removes it.
pub(in crate::logs::store) fn seal(hot: &Path) -> Result<Option<u32>> {
    let opened = File::options()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(hot.join(SEAL));
    let file = match opened {
        Ok(f) => f,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading the seal of {}", hot.display())),
    };
    let mut text = String::new();
    file.take(16).read_to_string(&mut text)?;
    Ok(seq_of(text.trim_end()))
}

/// Seals the hot day at `hot` for segment `seq`, durably: written aside,
/// synced and renamed into place.
pub(in crate::logs::store) fn write_seal(hot: &Path, seq: u32) -> Result<()> {
    let aside = hot.join(format!(".{SEAL}.tmp"));
    remove_file(&aside)?;
    let mut f = File::options()
        .write(true)
        .create_new(true)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(&aside)?;
    writeln!(f, "{seq}")?;
    f.sync_all()?;
    std::fs::rename(&aside, hot.join(SEAL))?;
    sync_dir(hot)
}

/// Removes the seal of the hot day at `hot`, if it has one.
pub(in crate::logs::store) fn remove_seal(hot: &Path) -> Result<()> {
    remove_file(&hot.join(SEAL))
}

/// Removes the file at `path` if there is one.
pub(in crate::logs::store) fn remove_file(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("removing {}", path.display())),
    }
}

/// Makes the entries of `dir` durable: a rename into it survives a crash
/// once this returns.
pub(in crate::logs::store) fn sync_dir(dir: &Path) -> Result<()> {
    File::open(dir)
        .and_then(|d| d.sync_all())
        .with_context(|| format!("syncing {}", dir.display()))
}

#[cfg(test)]
#[path = "cold_layout_tests.rs"]
mod tests;
