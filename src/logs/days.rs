//! The day indexes a search reads, kept open between searches.
//!
//! Opening a day index costs about a millisecond, so a search over 90 days
//! for a word that is rare paid about 90 ms to open them before it searched
//! anything. A day is opened again only when its `meta.json` changed, which
//! tantivy replaces on every commit.

use super::day_dirs::{self, Entry};
use anyhow::{Context, Result};
use jiff::civil::Date;
use std::{
    collections::HashMap,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex, PoisonError},
    time::SystemTime,
};
use tantivy::{Index, IndexReader, ReloadPolicy, Searcher};

/// Day indexes kept open at most. Each holds memory maps of its segment
/// files, and Linux allows a process 65,530 maps by default.
const KEEP_OPEN: usize = 400;

/// Decompressed docstore blocks a day keeps. A search reads at most 500
/// logs, mostly from one or two days, and tantivy's default of 100 blocks
/// per segment would let 400 open days hold gigabytes.
const DOC_BLOCKS: usize = 8;

/// One day index, read as it was when opened.
#[derive(Clone)]
pub(super) struct Day {
    pub(super) index: Index,
    pub(super) searcher: Searcher,
}

/// What identifies one version of a day's `meta.json`. A commit renames a
/// new file over it, so the inode changes even when the time and size do not.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Stamp {
    modified: SystemTime,
    len: u64,
    inode: u64,
}

impl Stamp {
    fn of(dir: &Path) -> Option<Self> {
        let m = std::fs::metadata(dir.join("meta.json")).ok()?;
        Some(Self {
            modified: m.modified().ok()?,
            len: m.len(),
            inode: m.ino(),
        })
    }
}

struct Open {
    stamp: Stamp,
    day: Day,
    used: u64,
}

#[derive(Default)]
struct Cache {
    days: HashMap<(PathBuf, Date), Open>,
    clock: u64,
}

static CACHE: LazyLock<Mutex<Cache>> = LazyLock::new(Mutex::default);

fn cache() -> std::sync::MutexGuard<'static, Cache> {
    CACHE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The hot days of the log root, oldest first. A stray is not one: see
/// `day_dirs`.
pub(super) fn list(root: &Path) -> Result<Vec<Date>> {
    let mut all: Vec<Date> = match std::fs::read_dir(root) {
        Ok(entries) => entries
            .filter_map(|e| match day_dirs::entry(&e.ok()?) {
                Entry::Day(day) => Some(day),
                Entry::Stray(_) | Entry::Other => None,
            })
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e.into()),
    };
    all.sort_unstable();
    Ok(all)
}

/// The hot days of the log root, oldest first, closing the open indexes
/// of days no longer there.
pub(super) fn current(root: &Path) -> Result<Vec<Date>> {
    let all = list(root)?;
    cache()
        .days
        .retain(|(r, d), _| r != root || all.binary_search(d).is_ok());
    Ok(all)
}

/// The searcher of the hot day `day`, none when it has no index.
pub(super) fn one(root: &Path, day: Date) -> Option<Result<Day>> {
    let dir = root.join(day.to_string());
    let stamp = Stamp::of(&dir)?;
    Some(open(root, day, &dir, stamp))
}

/// Drops the open index of a day retention is about to delete, so its
/// memory maps do not keep the deleted files on disk.
pub(super) fn forget(root: &Path, day: Date) {
    cache().days.remove(&(root.to_owned(), day));
}

/// The day at `dir`, from the cache when its `meta.json` is unchanged.
fn open(root: &Path, day: Date, dir: &Path, stamp: Stamp) -> Result<Day> {
    let key = (root.to_owned(), day);
    if let Some(day) = cache().get(&key, stamp) {
        return Ok(day);
    }
    let day = Day::open(dir)?;
    cache().insert(key, stamp, day.clone(), KEEP_OPEN);
    Ok(day)
}

impl Cache {
    fn get(&mut self, key: &(PathBuf, Date), stamp: Stamp) -> Option<Day> {
        self.clock += 1;
        let o = self.days.get_mut(key).filter(|o| o.stamp == stamp)?;
        o.used = self.clock;
        Some(o.day.clone())
    }

    /// Adds `day`, first closing the least recently used one when `keep`
    /// days are open.
    fn insert(&mut self, key: (PathBuf, Date), stamp: Stamp, day: Day, keep: usize) {
        if self.days.len() >= keep && !self.days.contains_key(&key) {
            let oldest = self.days.iter().min_by_key(|(_, o)| o.used);
            if let Some(k) = oldest.map(|(k, _)| k.clone()) {
                self.days.remove(&k);
            }
        }
        self.clock += 1;
        let used = self.clock;
        self.days.insert(key, Open { stamp, day, used });
    }
}

impl Day {
    fn open(dir: &Path) -> Result<Self> {
        let index = Index::open_in_dir(dir).with_context(|| format!("open {}", dir.display()))?;
        let reader: IndexReader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .doc_store_cache_num_blocks(DOC_BLOCKS)
            .try_into()?;
        let searcher = reader.searcher();
        Ok(Self { index, searcher })
    }
}

#[cfg(test)]
#[path = "days_tests.rs"]
mod tests;
