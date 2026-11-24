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
//! often the bigger it is, see [`pace`], also after the day closed: a
//! closed day's counters stay in memory until they are written and their
//! pace is over, so a day that opens again soon is not read again and its
//! pace holds. At most [`HELD_DAYS`] closed days stay, and at shutdown
//! every file that changed is written. Retention removes the counters with
//! the logs they count. Readers in the server process see the memory, so
//! they are as fresh as the index; readers in another process see the
//! files.
//!
//! Log text can come from a public DSN, so everything here is bounded per
//! project and day: [`MAX_PATTERNS`] patterns, then [`OTHER`], and past
//! [`FREE_PATTERNS`] only patterns seen [`ADMIT_AFTER`] times; [`MAX_CELLS`]
//! pairs of pattern and service, then [`OTHER_SERVICES`]; and the length of
//! each template, example and service name.

use super::{State, retention::Cut};
use crate::{
    logs::{
        LogLevel, LogRecord,
        patterns::{self, OTHER},
    },
    text::printable,
};
use anyhow::Result;
use jiff::civil::Date;
use serde::{Deserialize, Serialize};
use std::{
    borrow::Cow,
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
/// Patterns a project counts in one day as they come. A new pattern past
/// them counts under [`OTHER`] until it was seen [`ADMIT_AFTER`] times
/// that day, so lines seen once, junk or values the mask missed, leave
/// the rest of the room to lines that recur.
pub const FREE_PATTERNS: usize = 500;
/// Sightings that give a new pattern past [`FREE_PATTERNS`] its own count.
pub const ADMIT_AFTER: u8 = 3;
/// New patterns past [`FREE_PATTERNS`] whose sightings a project tracks.
const CANDIDATES: usize = 1000;
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
/// Days the writer closed whose counters it keeps in memory. Past them the
/// one closed longest ago goes, a day already written first, and a day not
/// written yet is written then whatever its pace.
const HELD_DAYS: usize = 3;
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
    /// The file could not be read, for a reason other than what it holds,
    /// or the day's files could not be listed, so it is never written over.
    blocked: bool,
    /// The counters started again from nothing that day, because a file
    /// of them could not be read, so they count less than the logs hold.
    restarted: bool,
    /// What was counted since the day's last commit. It joins `patterns`
    /// when the commit holds and goes with the logs when it fails, so no
    /// reader and no file sees a count of a log that search cannot find.
    pending: HashMap<u64, Entry>,
    /// Patterns in `pending` that `patterns` lacks, [`OTHER`] aside.
    added: usize,
    /// Pairs of pattern and service in `pending` that `patterns` lacks.
    added_cells: usize,
    /// Sightings of new patterns past [`FREE_PATTERNS`], see [`seen`].
    candidates: HashMap<u64, u8>,
}

/// The counters of one day.
#[derive(Default)]
pub(super) struct DayTally {
    pub(super) projects: HashMap<u64, ProjectDay>,
    /// Whether the last write of a file of the day failed.
    failing: bool,
    /// When the writer closed the day, `None` while it has the day open.
    closed: Option<Instant>,
    /// The day's directory could not be listed, so the writer cannot tell
    /// which files hold counters and writes none.
    blocked: bool,
}

pub(super) type Days = HashMap<Date, DayTally>;

/// The projects of a day whose file could not be read, with the error.
pub(super) type Unread = Vec<(u64, anyhow::Error)>;

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

/// `s` without control characters and marks that reorder text, cut to
/// `bytes`. Templates, examples and service names come from senders and
/// reach agents and terminals, also as JSON, which escapes only C0.
fn clean(s: &str, bytes: usize) -> Cow<'_, str> {
    match printable(cut(s, bytes), char::REPLACEMENT_CHARACTER) {
        Cow::Borrowed(s) => Cow::Borrowed(s),
        Cow::Owned(s) => Cow::Owned(cut(&s, bytes).to_owned()),
    }
}

/// Counts a sighting of `id`, a new pattern past [`FREE_PATTERNS`], and
/// returns whether it now has its own count. A full table takes one
/// sighting from each pattern it holds and drops those left with none, as
/// in the Misra-Gries count of frequent items: a pattern that recurs more
/// often than one new line in [`CANDIDATES`] gets in whatever comes between,
/// and the table costs at most one step per sighting on average.
fn seen(candidates: &mut HashMap<u64, u8>, id: u64) -> bool {
    if let Some(n) = candidates.get_mut(&id) {
        *n += 1;
        if *n < ADMIT_AFTER {
            return false;
        }
        candidates.remove(&id);
        return true;
    }
    if candidates.len() < CANDIDATES {
        candidates.insert(id, 1);
    } else {
        candidates.retain(|_, n| {
            *n -= 1;
            *n > 0
        });
    }
    false
}

impl ProjectDay {
    /// Counts one record as pending and returns the pattern it counts
    /// under: its own, or [`OTHER`] when the project has no room for one
    /// more pattern today or the pattern has not earned it yet, see
    /// [`FREE_PATTERNS`]. Pending patterns and pairs take room too, so the
    /// bounds hold whatever the commits do.
    fn count(&mut self, r: &LogRecord, pattern: patterns::Pattern, bytes: u64) -> u64 {
        let Self {
            patterns,
            cells,
            pending,
            added,
            added_cells,
            candidates,
            ..
        } = self;
        let known = patterns.contains_key(&pattern.id) || pending.contains_key(&pattern.id);
        let real = patterns.len() - usize::from(patterns.contains_key(&OTHER)) + *added;
        // A known pattern takes no sighting, so the traffic of known lines
        // cannot wear down the sightings of a new one.
        let room =
            known || real < FREE_PATTERNS || (real < MAX_PATTERNS && seen(candidates, pattern.id));
        let (id, template) = if room {
            (pattern.id, pattern.template)
        } else {
            (OTHER, String::new())
        };
        let done = patterns.get(&id);
        if done.is_none() && id != OTHER && !pending.contains_key(&id) {
            *added += 1;
        }
        let entry = pending.entry(id).or_insert_with(|| Entry {
            // A template is at most 12 tokens of 64 bytes, so even three
            // bytes for each of its bytes stay within what a file may hold.
            template: printable(&template, char::REPLACEMENT_CHARACTER).into_owned(),
            example: clean(&r.message, EXAMPLE_BYTES).into_owned(),
            services: HashMap::new(),
        });
        let has = |entry: &Entry, service: &str| {
            entry.services.contains_key(service)
                || done.is_some_and(|d| d.services.contains_key(service))
        };
        let named = clean(r.service.as_deref().unwrap_or(""), SERVICE_BYTES);
        let mut service: &str = &named;
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

    /// Whether the file is behind and may be written.
    fn due(&self) -> bool {
        self.dirty && !self.blocked
    }
}

impl DayTally {
    /// Counts one record as pending, see [`ProjectDay::count`].
    pub(super) fn count(&mut self, r: &LogRecord, pattern: patterns::Pattern, bytes: u64) -> u64 {
        let project = r.project_id.cast_unsigned();
        let blocked = self.blocked;
        let day = self.projects.entry(project).or_insert_with(|| ProjectDay {
            blocked,
            ..ProjectDay::default()
        });
        day.count(r, pattern, bytes)
    }

    /// Moves what was counted since the day's last commit into the
    /// counters, once that commit holds.
    pub(super) fn settle(&mut self) {
        self.projects.values_mut().for_each(ProjectDay::settle);
    }

    /// Whether counters of the day, of `project` alone when it is named,
    /// started again from nothing, see [`ProjectDay::restarted`].
    pub(super) fn restarted(&self, project: Option<u64>) -> bool {
        let restarted = |p: &ProjectDay| p.restarted;
        self.blocked
            || match project {
                Some(project) => self.projects.get(&project).is_some_and(restarted),
                None => self.projects.values().any(restarted),
            }
    }

    fn dirty(&self) -> bool {
        self.projects.values().any(ProjectDay::due)
    }

    /// Whether a file of the day was written so recently that writing it
    /// again at `now` would break its pace.
    fn pacing(&self, now: Instant) -> bool {
        let pacing = |p: &ProjectDay| p.next_save.is_some_and(|at| at > now);
        self.projects.values().any(pacing)
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
    /// named, empty when there are none yet. A file that cannot be read is
    /// left out and returned with its error. A project whose file was set
    /// aside has [`ProjectDay::restarted`].
    pub(super) fn load(dir: &Path, project: Option<u64>) -> Result<(Self, Unread)> {
        let named = |p: &u64| project.is_none_or(|named| named == *p);
        let (projects, aside) = file::projects(dir)?;
        let (mut day, mut unread) = (Self::default(), Vec::new());
        for project in projects.into_iter().filter(named) {
            match file::load(dir, project) {
                Ok(Some(counters)) => drop(day.projects.insert(project, counters)),
                Ok(None) => {}
                Err(e) => unread.push((project, e)),
            }
        }
        for project in aside.into_iter().filter(named) {
            day.projects.entry(project).or_default().restarted = true;
        }
        Ok((day, unread))
    }

    /// Notes whether a write of the day failed, and returns whether to
    /// report it: only the first failure of a run is, so a disk that stays
    /// broken does not fill the log.
    fn note(&mut self, failed: bool) -> bool {
        let first = failed && !self.failing;
        self.failing = failed;
        first
    }

    /// Writes the projects that changed, whatever their pace.
    fn save(&mut self, dir: &Path) -> Result<()> {
        let mut first = None;
        for (project, counters) in self.projects.iter_mut().filter(|(_, p)| p.due()) {
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
        let tally = tallies.entry(day).or_insert_with(|| open(day, &dir));
        tally.closed = None;
        tally.count(r, pattern, bytes)
    }

    /// Writes the files of the projects that changed and are due at `now`,
    /// and lets go of the days the writer no longer has open once they are
    /// written and their pace is over. The files are written outside the
    /// lock, so readers do not wait for the disk. A file that could not be
    /// written is tried again at its next turn.
    pub(super) fn flush_tallies(&mut self, now: Instant) {
        let (mut due, mut failed) = (Vec::new(), Vec::new());
        for (day, tally) in self.tallies().iter_mut() {
            let ready = |p: &&mut ProjectDay| p.due() && p.next_save.is_none_or(|at| at <= now);
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
        let mut written: HashMap<Date, Option<anyhow::Error>> = HashMap::new();
        for (day, project, bytes) in due {
            let saved = file::save(&self.root.join(day.to_string()), project, &bytes);
            let first = written.entry(day).or_default();
            if let Err(e) = saved {
                first.get_or_insert(e);
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
        for (day, e) in written {
            if let Some(tally) = tallies.get_mut(&day)
                && tally.note(e.is_some())
                && let Some(e) = e
            {
                report(day, "writing", &e);
            }
        }
        tallies.retain(|day, t| t.dirty() || t.pacing(now) || open.contains(day));
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

    /// Keeps the counters of `day`, which the writer closed, for the flush
    /// to write at their pace, see [`HELD_DAYS`]. A sender that sends to
    /// one day after another so makes no file be written more often than
    /// when it sends to one day.
    pub(super) fn close_tally(&mut self, day: Date) {
        let mut tallies = self.tallies();
        if let Some(tally) = tallies.get_mut(&day) {
            tally.closed = Some(Instant::now());
        }
        let mut closed: Vec<(bool, Instant, Date)> = tallies
            .iter()
            .filter_map(|(d, t)| Some((t.dirty(), t.closed?, *d)))
            .collect();
        drop(tallies);
        closed.sort_unstable();
        let over = closed.len().saturating_sub(HELD_DAYS);
        for (_, _, day) in closed.into_iter().take(over) {
            self.write_tally(day);
        }
    }

    /// Writes the counters of `day` whatever their pace and lets go of
    /// them. Counters it could not write it keeps, and the flush tries
    /// again, as for a day open.
    pub(super) fn write_tally(&mut self, day: Date) {
        let dir = self.root.join(day.to_string());
        let mut tallies = self.tallies();
        let Some(tally) = tallies.get_mut(&day) else {
            return;
        };
        match tally.save(&dir) {
            Ok(()) => drop(tallies.remove(&day)),
            Err(e) if tally.note(true) => report(day, "writing", &e),
            Err(_) => {}
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

/// The counters of `day` in `dir`, for the writer. A file it cannot read
/// starts its project again from nothing. A damaged file is set aside. A
/// file the disk or its rights kept from being read, or one it cannot set
/// aside, is never written over, and neither is any file of a day whose
/// files it cannot list.
fn open(day: Date, dir: &Path) -> DayTally {
    let (mut tally, unread) = match DayTally::load(dir, None) {
        Ok(read) => read,
        Err(e) => {
            report(day, "reading", &e.context("no file of the day is written"));
            return DayTally {
                blocked: true,
                ..DayTally::default()
            };
        }
    };
    for (project, e) in unread {
        let counters = tally.projects.entry(project).or_default();
        counters.restarted = true;
        let e = if file::damaged(&e) {
            match file::set_aside(dir, project) {
                Ok(to) => e.context(format!("set aside as {}", to.display())),
                Err(moved) => {
                    counters.blocked = true;
                    e.context(format!("left alone and not written over: {moved:#}"))
                }
            }
        } else {
            counters.blocked = true;
            e.context("left alone and not written over")
        };
        report(day, "reading", &e);
    }
    if let Err(e) = file::remove_temporary(dir) {
        report(day, "cleaning up", &e);
    }
    tally
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
