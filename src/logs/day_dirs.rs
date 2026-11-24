//! The day directories of the log root, and what only looks like one.
//!
//! A day is a real directory named `YYYY-MM-DD`. A symlink or a file with
//! that name, or a directory with another name for a date, such as
//! `+002026-01-05`, is a stray: it is not searched, written or deleted,
//! retention reports it, and `davy doctor` warns about it. Following a
//! symlink would let a deletion leave the log root, and a second name for
//! a day would split its logs in two.

use super::parse_day;
use anyhow::Result;
use jiff::civil::Date;
use serde::Serialize;
use std::{fs::DirEntry, path::Path};

/// What an entry of the log root is.
pub(super) enum Entry {
    Day(Date),
    Stray(Stray),
    /// Anything not named like a date, such as `.trash`.
    Other,
}

/// An entry of the log root named like a day that is not one.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Stray {
    pub name: String,
    pub why: &'static str,
}

/// What `entry` of the log root is. The type of a symlink is the link's,
/// not its target's.
pub(super) fn entry(entry: &DirEntry) -> Entry {
    let name = entry.file_name().to_string_lossy().into_owned();
    let stray = |why| {
        Entry::Stray(Stray {
            name: name.clone(),
            why,
        })
    };
    if let Some(day) = parse_day(&name) {
        return match entry.file_type() {
            Ok(t) if t.is_dir() => Entry::Day(day),
            Ok(t) if t.is_symlink() => stray("is a symlink, not the day's directory"),
            Ok(_) => stray("is not a directory"),
            Err(_) => stray("cannot be read"),
        };
    }
    if name.parse::<Date>().is_ok() {
        return stray("names a date, but a day's directory is named YYYY-MM-DD");
    }
    Entry::Other
}

/// The strays in the log root `root`, by name.
pub fn strays(root: &Path) -> Result<Vec<Stray>> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut found = Vec::new();
    for e in entries {
        if let Entry::Stray(s) = entry(&e?) {
            found.push(s);
        }
    }
    found.sort_unstable_by(|a, b| a.name.cmp(&b.name));
    Ok(found)
}

/// Whether `dir` exists as a real directory. A symlink or anything else
/// there is an error, so nothing is written, renamed or deleted through it.
pub(super) fn real_dir(dir: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(dir) {
        Ok(m) if m.is_dir() => Ok(true),
        Ok(_) => anyhow::bail!(
            "{} is not a real directory, so Davy leaves it alone",
            dir.display()
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}
