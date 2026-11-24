//! Counters of log patterns: records and raw bytes per UTC day, project,
//! pattern, service and level, with one example message per pattern.
//!
//! The log writer counts each record as it indexes it and stamps the record
//! with the pattern it was counted under, so the counters and a search by
//! pattern agree. A count is pending until the commit of its day holds.
//!
//! A day's counters live in memory while the writer has the day open, and
//! in a file per project in the day's directory, see [`file`]. A project's
//! file is written when it changed, at most every [`FLUSH_EVERY`] and less
//! often the bigger it is, see [`pace`], and when the day closes. Retention
//! removes the counters with the logs they count. Readers in the server
//! process see the memory, so they are as fresh as the index; readers in
//! another process see the files.
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
use anyhow::Result;
use jiff::civil::Date;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, Mutex, PoisonError, Weak},
    time::{Duration, Instant},
};

#[path = "tally_file.rs"]
pub(super) mod file;

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
/// Bytes per second that rewriting one project's file may cost.
const SAVE_RATE: usize = 10_000;
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

#[derive(Default)]
pub(super) struct ProjectDay {
    pub(super) patterns: HashMap<u64, Entry>,
    cells: usize,
    /// Whether `patterns` changed since the file was written.
    dirty: bool,
    /// When the file may be written again, see [`pace`].
    next_save: Option<Instant>,
    /// What was counted since the day's last commit. It joins `patterns`
    /// when the commit holds and goes with the logs when it fails, so no
    /// reader and no file sees a count of a log that search cannot find.
    pending: HashMap<u64, Entry>,
    /// Patterns in `pending` that `patterns` lacks, [`OTHER`] aside.
    added: usize,
    /// Pairs of pattern and service in `pending` that `patterns` lacks.
    added_cells: usize,
}

/// The counters of one day.
#[derive(Default)]
pub(super) struct DayTally {
    pub(super) projects: HashMap<u64, ProjectDay>,
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

impl ProjectDay {
    /// Counts one record as pending and returns the pattern it counts
    /// under: its own, or [`OTHER`] when the project has no room for one
    /// more pattern today. Pending patterns and pairs take room too, so the
    /// bounds hold whatever the commits do.
    fn count(&mut self, r: &LogRecord, pattern: patterns::Pattern, bytes: u64) -> u64 {
        let Self {
            patterns,
            cells,
            pending,
            added,
            added_cells,
            ..
        } = self;
        let known = patterns.contains_key(&pattern.id) || pending.contains_key(&pattern.id);
        let real = patterns.len() - usize::from(patterns.contains_key(&OTHER)) + *added;
        let (id, template) = if known || real < MAX_PATTERNS {
            (pattern.id, pattern.template)
        } else {
            (OTHER, String::new())
        };
        let done = patterns.get(&id);
        if done.is_none() && id != OTHER && !pending.contains_key(&id) {
            *added += 1;
        }
        let entry = pending.entry(id).or_insert_with(|| Entry {
            template,
            example: cut(&r.message, EXAMPLE_BYTES).to_owned(),
            services: HashMap::new(),
        });
        let has = |entry: &Entry, service: &str| {
            entry.services.contains_key(service)
                || done.is_some_and(|d| d.services.contains_key(service))
        };
        let mut service = cut(r.service.as_deref().unwrap_or(""), SERVICE_BYTES);
        if !has(entry, service) {
            if *cells + *added_cells >= MAX_CELLS {
                service = OTHER_SERVICES;
            }
            if !has(entry, service) {
                *added_cells += 1;
            }
        }
        if !entry.services.contains_key(service) {
            entry.services.insert(service.into(), Levels::default());
        }
        if let Some(levels) = entry.services.get_mut(service) {
            let slot = &mut levels[level_index(r.level)];
            slot[0] = slot[0].saturating_add(1);
            slot[1] = slot[1].saturating_add(bytes);
        }
        id
    }

    /// Moves the pending counts into the counters.
    fn settle(&mut self) {
        let Self {
            patterns,
            cells,
            pending,
            added,
            added_cells,
            ..
        } = self;
        let any = !pending.is_empty();
        for (id, entry) in pending.drain() {
            let Entry {
                template,
                example,
                services,
            } = entry;
            let into = patterns.entry(id).or_insert_with(|| Entry {
                template,
                example,
                services: HashMap::new(),
            });
            for (service, levels) in services {
                let slot = into.services.entry(service).or_insert_with(|| {
                    *cells += 1;
                    Levels::default()
                });
                for (sum, more) in slot.iter_mut().zip(levels) {
                    sum[0] = sum[0].saturating_add(more[0]);
                    sum[1] = sum[1].saturating_add(more[1]);
                }
            }
        }
        (*added, *added_cells) = (0, 0);
        self.dirty |= any;
    }

    fn discard(&mut self) {
        self.pending.clear();
        (self.added, self.added_cells) = (0, 0);
    }
}

impl DayTally {
    /// Counts one record as pending, see [`ProjectDay::count`].
    pub(super) fn count(&mut self, r: &LogRecord, pattern: patterns::Pattern, bytes: u64) -> u64 {
        let project = r.project_id.cast_unsigned();
        let day = self.projects.entry(project).or_default();
        day.count(r, pattern, bytes)
    }

    /// Moves what was counted since the day's last commit into the
    /// counters, once that commit holds.
    pub(super) fn settle(&mut self) {
        self.projects.values_mut().for_each(ProjectDay::settle);
    }

    fn dirty(&self) -> bool {
        self.projects.values().any(|p| p.dirty)
    }

    /// Forgets what was counted since the day's last commit, which failed.
    pub(super) fn discard(&mut self) {
        self.projects.values_mut().for_each(ProjectDay::discard);
    }

    /// Drops the counters of the projects `cut` deletes the logs of.
    fn cut(&mut self, cut: &Cut) {
        self.projects.retain(|p, _| match cut {
            Cut::Only(gone) => !gone.contains(p),
            Cut::AllBut(kept) => kept.contains(p),
        });
    }

    /// The counters of the day in `dir`, of `project` alone when it is
    /// named, empty when there are none yet.
    pub(super) fn load(dir: &Path, project: Option<u64>) -> Result<Self> {
        let projects = match project {
            Some(project) => vec![project],
            None => file::projects(dir)?,
        };
        let mut day = Self::default();
        for project in projects {
            if let Some(counters) = file::load(dir, project)? {
                day.projects.insert(project, counters);
            }
        }
        Ok(day)
    }

    /// Writes the projects that changed, whatever their pace.
    fn save(&mut self, dir: &Path) -> Result<()> {
        let mut first = None;
        for (project, counters) in self.projects.iter_mut().filter(|(_, p)| p.dirty) {
            match file::encode(counters).and_then(|bytes| file::save(dir, *project, &bytes)) {
                Ok(()) => counters.dirty = false,
                Err(e) => drop(first.get_or_insert(e)),
            }
        }
        first.map_or(Ok(()), Err)
    }
}

/// How long a project's file of `len` bytes waits before it is written
/// again: [`FLUSH_EVERY`], or longer so that rewriting it costs at most
/// [`SAVE_RATE`] bytes a second.
pub(super) fn pace(len: usize) -> Duration {
    let secs = u64::try_from(len / SAVE_RATE).unwrap_or(u64::MAX);
    FLUSH_EVERY.max(Duration::from_secs(secs))
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
            DayTally::load(&dir, None).unwrap_or_else(|e| {
                report(day, "reading", &e);
                DayTally::default()
            })
        });
        tally.count(r, pattern, bytes)
    }

    /// Writes the files of the projects that changed and are due at `now`,
    /// and lets go of the days the writer no longer has open. The files are
    /// written outside the lock, so readers do not wait for the disk.
    pub(super) fn flush_tallies(&mut self, now: Instant) {
        let mut due = Vec::new();
        for (day, tally) in self.tallies().iter_mut() {
            let ready = |p: &&mut ProjectDay| p.dirty && p.next_save.is_none_or(|at| at <= now);
            for (project, counters) in tally.projects.iter_mut().filter(|(_, p)| ready(p)) {
                match file::encode(counters) {
                    Ok(bytes) => {
                        counters.dirty = false;
                        counters.next_save = Some(now + pace(bytes.len()));
                        due.push((*day, *project, bytes));
                    }
                    Err(e) => report(*day, "writing", &e),
                }
            }
        }
        let mut failed = Vec::new();
        for (day, project, bytes) in due {
            if let Err(e) = file::save(&self.root.join(day.to_string()), project, &bytes) {
                report(day, "writing", &e);
                failed.push((day, project));
            }
        }
        let open: Vec<Date> = self.open.keys().copied().collect();
        let mut tallies = self.tallies();
        for (day, project) in failed {
            if let Some(counters) = tallies
                .get_mut(&day)
                .and_then(|t| t.projects.get_mut(&project))
            {
                counters.dirty = true;
            }
        }
        tallies.retain(|day, t| t.dirty() || open.contains(day));
    }

    /// Moves the pending counts of `day` into its counters.
    pub(super) fn settle_tally(&mut self, day: Date) {
        if let Some(tally) = self.tallies().get_mut(&day) {
            tally.settle();
        }
    }

    /// Forgets the pending counts of `day`, whose logs were lost.
    pub(super) fn discard_tally(&mut self, day: Date) {
        if let Some(tally) = self.tallies().get_mut(&day) {
            tally.discard();
        }
    }

    /// Writes the counters of `day` and lets go of them.
    pub(super) fn close_tally(&mut self, day: Date) {
        let dir = self.root.join(day.to_string());
        let mut tallies = self.tallies();
        if let Some(mut tally) = tallies.remove(&day)
            && let Err(e) = tally.save(&dir)
        {
            report(day, "writing", &e);
        }
    }

    /// Forgets the counters of `day`, whose directory goes.
    pub(super) fn drop_tally(&mut self, day: Date) {
        self.tallies().remove(&day);
    }

    /// Drops what `cut` deleted from `day` from its counters, in memory
    /// and on disk.
    pub(super) fn cut_tally(&mut self, day: Date, cut: &Cut) -> Result<()> {
        if let Some(tally) = self.tallies().get_mut(&day) {
            tally.cut(cut);
        }
        file::remove(&self.root.join(day.to_string()), cut)
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
