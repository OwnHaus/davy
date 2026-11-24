//! Counters of log patterns: records and raw bytes per UTC day, project,
//! pattern, service and level, with one example message per pattern.
//!
//! The log writer counts each record as it indexes it and stamps the record
//! with the pattern it was counted under, so the counters and a search by
//! pattern agree. A day's counters live in memory while the writer has the
//! day open, and in `patterns.json` in the day's directory, written every
//! [`FLUSH_EVERY`] and when the day closes. Retention removes them with the
//! logs they count. Readers in the server process see the memory, so they
//! are as fresh as the index; readers in another process see the file.
//!
//! Log text can come from a public DSN, so everything here is bounded per
//! project and day: [`MAX_PATTERNS`] patterns, then [`OTHER`]; [`MAX_CELLS`]
//! pairs of pattern and service, then [`OTHER_SERVICES`]; and the length of
//! each template, example and service name.

use super::{State, retention::Cut};
use crate::logs::{
    LogLevel, LogRecord,
    patterns::{self, OTHER},
};
use anyhow::{Context, Result};
use jiff::civil::Date;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs::File,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, Mutex, PoisonError, Weak},
    time::Duration,
};

/// Patterns a project counts in one day. The rest count under [`OTHER`].
pub const MAX_PATTERNS: usize = 1000;
/// Pairs of pattern and service a project counts in one day. A new service
/// of a pattern past it counts under [`OTHER_SERVICES`].
pub const MAX_CELLS: usize = 10_000;
pub const OTHER_SERVICES: &str = "(other services)";
/// Bytes of a service name the counters keep.
pub const SERVICE_BYTES: usize = 128;
/// Bytes of the example message a pattern keeps.
pub const EXAMPLE_BYTES: usize = 256;
pub(super) const FLUSH_EVERY: Duration = Duration::from_secs(10);
pub(super) const FILE: &str = "patterns.json";
const VERSION: u32 = 1;
const LEVELS: usize = 6;

/// Records and bytes of one service of a pattern, per level from trace to
/// fatal.
pub(super) type Levels = [[u64; 2]; LEVELS];

#[derive(Serialize, Deserialize)]
pub(super) struct Entry {
    pub(super) template: String,
    pub(super) example: String,
    /// By service name, `""` for records without one.
    pub(super) services: HashMap<Box<str>, Levels>,
}

#[derive(Default, Serialize, Deserialize)]
pub(super) struct ProjectDay {
    pub(super) patterns: HashMap<u64, Entry>,
    #[serde(skip)]
    cells: usize,
}

/// The counters of one day.
#[derive(Default, Serialize, Deserialize)]
pub(super) struct DayTally {
    version: u32,
    pub(super) projects: HashMap<u64, ProjectDay>,
    #[serde(skip)]
    dirty: bool,
}

pub(super) type Days = HashMap<Date, DayTally>;

/// The counters each log writer of this process holds, by its root, so a
/// reader sees counts that are not on the disk yet.
static LIVE: LazyLock<Mutex<HashMap<PathBuf, Weak<Mutex<Days>>>>> = LazyLock::new(Mutex::default);

/// New counters for the writer of `root`, seen by readers of `root`.
pub(super) fn register(root: &Path) -> Arc<Mutex<Days>> {
    let days = Arc::default();
    let mut live = LIVE.lock().unwrap_or_else(PoisonError::into_inner);
    live.retain(|_, w| w.strong_count() > 0);
    live.insert(root.to_path_buf(), Arc::downgrade(&days));
    days
}

/// The counters the writer of `root` holds in memory, if it runs here.
pub(super) fn live(root: &Path) -> Option<Arc<Mutex<Days>>> {
    let live = LIVE.lock().unwrap_or_else(PoisonError::into_inner);
    live.get(root).and_then(Weak::upgrade)
}

fn cut(s: &str, bytes: usize) -> &str {
    &s[..s.floor_char_boundary(bytes)]
}

impl DayTally {
    /// Counts one record and returns the pattern it counts under: its own,
    /// or [`OTHER`] when the project has no room for one more pattern today.
    pub(super) fn count(&mut self, r: &LogRecord, pattern: patterns::Pattern, bytes: u64) -> u64 {
        self.dirty = true;
        let day = self
            .projects
            .entry(r.project_id.cast_unsigned())
            .or_default();
        let known = day.patterns.contains_key(&pattern.id);
        let real = day.patterns.len() - usize::from(day.patterns.contains_key(&OTHER));
        let (id, template) = if known || real < MAX_PATTERNS {
            (pattern.id, pattern.template)
        } else {
            (OTHER, String::new())
        };
        let entry = day.patterns.entry(id).or_insert_with(|| Entry {
            template,
            example: cut(&r.message, EXAMPLE_BYTES).to_owned(),
            services: HashMap::new(),
        });
        let mut service = cut(r.service.as_deref().unwrap_or(""), SERVICE_BYTES);
        if !entry.services.contains_key(service) {
            if day.cells >= MAX_CELLS {
                service = OTHER_SERVICES;
            }
            if !entry.services.contains_key(service) {
                day.cells += 1;
                entry.services.insert(service.into(), Levels::default());
            }
        }
        if let Some(levels) = entry.services.get_mut(service) {
            let slot = &mut levels[level_index(r.level)];
            slot[0] += 1;
            slot[1] += bytes;
        }
        id
    }

    /// Drops the counters of the projects `cut` deletes the logs of.
    fn cut(&mut self, cut: &Cut) {
        let before = self.projects.len();
        self.projects.retain(|p, _| match cut {
            Cut::Only(gone) => !gone.contains(p),
            Cut::AllBut(kept) => kept.contains(p),
        });
        self.dirty |= self.projects.len() != before;
    }

    /// The counters of the day in `dir`, empty when it has none yet.
    pub(super) fn load(dir: &Path) -> Result<Self> {
        let text = match std::fs::read(dir.join(FILE)) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e.into()),
        };
        let mut day: Self = serde_json::from_slice(&text)
            .with_context(|| format!("reading {}", dir.join(FILE).display()))?;
        anyhow::ensure!(day.version == VERSION, "unknown version {}", day.version);
        for project in day.projects.values_mut() {
            project.cells = project.patterns.values().map(|e| e.services.len()).sum();
        }
        Ok(day)
    }

    /// Writes the counters to `dir`, whole or not at all.
    fn save(&mut self, dir: &Path) -> Result<()> {
        self.version = VERSION;
        write_durably(dir, FILE, &serde_json::to_vec(self)?)?;
        self.dirty = false;
        Ok(())
    }
}

/// Replaces `name` in `dir` with `bytes` so that a crash or a power loss
/// leaves the old file or the new one, never a part. The temporary file
/// has a name of its own, so a file left at that name by a crash or put
/// there by anyone is neither followed nor in the way.
fn write_durably(dir: &Path, name: &str, bytes: &[u8]) -> Result<()> {
    let tmp = dir.join(format!("{name}.{}.tmp", uuid::Uuid::new_v4()));
    let written = (|| -> io::Result<()> {
        let mut file = File::options().write(true).create_new(true).open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&tmp, dir.join(name))
    })();
    if let Err(e) = written {
        let e = anyhow::Error::from(e).context(format!("writing {}", dir.join(name).display()));
        return Err(match std::fs::remove_file(&tmp) {
            Err(left) if left.kind() != io::ErrorKind::NotFound => {
                e.context(format!("{} is left: {left}", tmp.display()))
            }
            _ => e,
        });
    }
    // The rename is durable only once the directory that holds it is.
    File::open(dir)?.sync_all()?;
    Ok(())
}

pub(super) fn level_index(level: LogLevel) -> usize {
    level as usize - 1
}

/// What a record weighs as JSON text: its message, ids and attributes.
pub(super) fn raw_bytes(r: &LogRecord) -> u64 {
    struct Counter(usize);
    impl Write for Counter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0 += buf.len();
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut attributes = Counter(0);
    if !r.attributes.is_empty() && serde_json::to_writer(&mut attributes, &r.attributes).is_err() {
        attributes.0 = 0;
    }
    let ids = [&r.service, &r.trace_id, &r.span_id];
    let ids: usize = ids.into_iter().flatten().map(String::len).sum();
    u64::try_from(r.message.len() + ids + attributes.0).unwrap_or(u64::MAX)
}

impl State {
    fn tallies(&self) -> std::sync::MutexGuard<'_, Days> {
        self.tallies.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Counts `r` in `day` and returns the pattern to stamp on it.
    pub(super) fn tally(&mut self, day: Date, r: &LogRecord) -> u64 {
        let pattern = patterns::of(&r.message);
        let bytes = raw_bytes(r);
        let dir = self.root.join(day.to_string());
        let mut tallies = self.tallies();
        let tally = tallies.entry(day).or_insert_with(|| {
            DayTally::load(&dir).unwrap_or_else(|e| {
                report(day, "reading", &e);
                DayTally::default()
            })
        });
        tally.count(r, pattern, bytes)
    }

    /// Writes the counters that changed, and lets go of the days the writer
    /// no longer has open.
    pub(super) fn flush_tallies(&mut self) {
        let root = self.root.clone();
        let open: Vec<Date> = self.open.keys().copied().collect();
        let mut tallies = self.tallies();
        for (day, tally) in tallies.iter_mut().filter(|(_, t)| t.dirty) {
            if let Err(e) = tally.save(&root.join(day.to_string())) {
                report(*day, "writing", &e);
            }
        }
        tallies.retain(|day, t| t.dirty || open.contains(day));
    }

    /// Writes the counters of `day` and lets go of them.
    pub(super) fn close_tally(&mut self, day: Date) {
        let dir = self.root.join(day.to_string());
        let mut tallies = self.tallies();
        if let Some(mut tally) = tallies.remove(&day)
            && tally.dirty
            && let Err(e) = tally.save(&dir)
        {
            report(day, "writing", &e);
        }
    }

    /// Forgets the counters of `day`, whose directory goes.
    pub(super) fn drop_tally(&mut self, day: Date) {
        self.tallies().remove(&day);
    }

    /// Drops what `cut` deleted from `day` from its counters.
    pub(super) fn cut_tally(&mut self, day: Date, cut: &Cut) -> Result<()> {
        let dir = self.root.join(day.to_string());
        let mut tallies = self.tallies();
        if let Some(tally) = tallies.get_mut(&day) {
            tally.cut(cut);
            return Ok(());
        }
        let mut tally = DayTally::load(&dir)?;
        tally.cut(cut);
        if tally.dirty {
            tally.save(&dir)?;
        }
        Ok(())
    }
}

#[expect(
    clippy::print_stderr,
    reason = "the server has no logger yet, stderr is its log"
)]
fn report(day: Date, what: &str, e: &anyhow::Error) {
    eprintln!("log writer: {what} the patterns of {day}: {e:#}");
}

#[cfg(test)]
#[path = "tally_tests.rs"]
mod tests;
