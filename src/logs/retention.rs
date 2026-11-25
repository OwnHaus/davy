//! Deletes the logs each project keeps no longer.
//!
//! Logs live in one index per UTC day that holds every project. A day older
//! than the longest retention of all is deleted whole, which is all there is
//! to do while no project has a setting of its own. A younger day loses the
//! logs of each project whose retention it outlived: tantivy marks them
//! deleted, and a merge rewrites the segments that held any, so they leave
//! the disk and not only the results. A day left with no logs is deleted.
//!
//! The settings are read at the start of each pass and again right before
//! each day is deleted from, under the lock a server takes while it saves a
//! change and the lock on the data directory the CLI takes. A change that
//! was saved is therefore never followed by a deletion under the setting it
//! replaced, and a pass that cannot read the settings deletes nothing.
//!
//! The pass remembers what it took from each day, so a day is read again
//! only when a setting changed, the day outlived another project's
//! retention, or a log was written to it. A pass that has deleted for
//! longer than its budget stops, and the next one goes on, so writes wait
//! at most about one day's merge.

use super::{
    DAY_NS, Fields, State, WRITER_HEAP,
    day_dirs::{self, Entry, Stray, real_dir},
    days, parse_day,
};
use crate::{
    retention::Lock,
    time::{day_of, now_ns},
};
use anyhow::{Context, Result};
use jiff::civil::Date;
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    time::{Duration, Instant},
};
use tantivy::{
    Index, IndexReader, IndexWriter, ReloadPolicy, Term,
    collector::Count,
    index::SegmentId,
    indexer::NoMergePolicy,
    query::{AllQuery, BooleanQuery, Occur, Query, TermQuery},
    schema::IndexRecordOption,
};

/// Where the log writer reads the projects that keep logs for a number of
/// days of their own, and the lock a change of that takes.
pub struct Overrides {
    read: Box<dyn FnMut() -> Result<HashMap<u64, u32>> + Send>,
    lock: Lock,
}

impl Overrides {
    /// Settings that `read` returns by project id, and that change only
    /// while `lock` is held.
    pub fn new(
        read: impl FnMut() -> Result<HashMap<u64, u32>> + Send + 'static,
        lock: Lock,
    ) -> Self {
        Self {
            read: Box::new(read),
            lock,
        }
    }
}

/// Overrides for a server where no project has a setting of its own.
pub fn no_overrides() -> Overrides {
    Overrides::new(|| Ok(HashMap::new()), Lock::default())
}

/// How many days of logs each project keeps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Policy {
    default: u32,
    projects: HashMap<u64, u32>,
}

impl Policy {
    pub(super) fn new(default: u32, projects: HashMap<u64, u32>) -> Self {
        Self { default, projects }
    }

    pub(super) fn with_projects(&self, projects: HashMap<u64, u32>) -> Self {
        Self::new(self.default, projects)
    }

    /// The days `project` keeps.
    pub(super) fn days(&self, project: u64) -> u32 {
        self.projects.get(&project).copied().unwrap_or(self.default)
    }

    /// What `day` holds that is past its project's retention at `now`.
    fn cut(&self, day: Date, now: i64) -> Cut {
        let expired = |days: u32| day < day_of(now - DAY_NS * i64::from(days));
        let projects = |gone: bool| {
            let p = self
                .projects
                .iter()
                .filter(move |(_, d)| expired(**d) == gone);
            p.map(|(p, _)| *p).collect()
        };
        if expired(self.default) {
            Cut::AllBut(projects(false))
        } else {
            Cut::Only(projects(true))
        }
    }
}

/// The logs to delete from one day.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Cut {
    /// The logs of these projects.
    Only(BTreeSet<u64>),
    /// Every log but those of these projects, including projects that are
    /// gone.
    AllBut(BTreeSet<u64>),
}

impl Cut {
    fn query(&self, f: &Fields) -> Box<dyn Query> {
        let project = |p: &u64| -> Box<dyn Query> {
            let term = Term::from_field_u64(f.project, *p);
            Box::new(TermQuery::new(term, IndexRecordOption::Basic))
        };
        let clauses = match self {
            Self::Only(gone) => gone.iter().map(|p| (Occur::Should, project(p))).collect(),
            Self::AllBut(kept) => std::iter::once((Occur::Must, Box::new(AllQuery) as _))
                .chain(kept.iter().map(|p| (Occur::MustNot, project(p))))
                .collect(),
        };
        Box::new(BooleanQuery::new(clauses))
    }
}

/// What deleting from one day left to do.
enum Deleted {
    /// Nothing in the day matched.
    Nothing,
    /// The whole day went.
    Day,
    /// Logs are marked deleted and committed, and wait for the merge that
    /// takes them off the disk.
    Marked {
        index: Index,
        writer: Box<IndexWriter>,
        reader: IndexReader,
    },
}

impl State {
    /// Reads the projects' own settings into the policy.
    fn refresh(&mut self) -> Result<()> {
        let projects = (self.overrides.read)().context("reading the retention settings")?;
        self.policy = self.policy.with_projects(projects);
        Ok(())
    }

    /// Deletes what expired at `now`, for at most about `budget` beyond the
    /// first day it deletes from or removes; the rest waits for the next
    /// call. A day that fails is reported and skipped, so one broken day
    /// cannot stop the retention of every day after it. Settings that cannot
    /// be read stop the pass before it deletes anything.
    pub(super) fn retention(&mut self, now: i64, budget: Duration) -> Result<()> {
        let deadline = Instant::now().checked_add(budget);
        self.refresh()?;
        self.empty_trash();
        let (mut found, mut strays) = (Vec::new(), HashSet::new());
        for entry in std::fs::read_dir(&self.root)? {
            // A symlink is not a day, so a deletion never leaves the root.
            match entry.map(|e| day_dirs::entry(&e)) {
                Ok(Entry::Day(day)) => found.push(day),
                Ok(Entry::Stray(stray)) => {
                    if self.strays.insert(stray.name.clone()) {
                        report_stray(&self.root, &stray);
                    }
                    strays.insert(stray.name);
                }
                Ok(Entry::Other) => {}
                Err(e) => report_entry(&e.into()),
            }
        }
        // A stray that was moved away and comes back is reported again.
        self.strays.retain(|name| strays.contains(name));
        found.sort_unstable();
        self.swept.retain(|day, _| found.contains(day));
        self.uncut.retain(|day, _| found.contains(day));
        for day in found {
            if let Err(e) = self.recut(day) {
                report(day, &e);
            }
            match self.sweep_day(day, now)? {
                Ok(wrote) => {
                    if wrote && deadline.is_some_and(|d| Instant::now() >= d) {
                        return Ok(());
                    }
                }
                Err(e) => report(day, &e),
            }
        }
        Ok(())
    }

    /// Whether `cut` would delete from `day` what was not deleted before.
    fn needs(&self, day: Date, cut: &Cut) -> bool {
        match cut {
            Cut::Only(gone) if gone.is_empty() => false,
            _ => self.swept.get(&day) != Some(cut),
        }
    }

    /// Tries again the cuts of counters of `day` that failed after their
    /// logs were deleted, whatever the settings say now: those logs are
    /// gone under any setting. It needs no lock, as only this thread writes
    /// counters, and a log written to the day since drops the cuts.
    fn recut(&mut self, day: Date) -> Result<()> {
        let Some(cuts) = self.uncut.remove(&day) else {
            return Ok(());
        };
        let (mut left, mut first) = (Vec::new(), None);
        for cut in cuts {
            if let Err(e) = self.cut_tally(day, &cut) {
                first.get_or_insert(e);
                left.push(cut);
            }
        }
        if !left.is_empty() {
            self.uncut.insert(day, left);
        }
        first.map_or(Ok(()), Err)
    }

    /// Deletes what expired at `now` from `day`. Returns whether it wrote
    /// to the disk, which is what costs time. The outer error is settings
    /// that cannot be read, which stops the pass; the inner one is this
    /// day's.
    fn sweep_day(&mut self, day: Date, now: i64) -> Result<Result<bool>> {
        if !self.needs(day, &self.policy.cut(day, now)) {
            return Ok(Ok(false));
        }
        // An index has one writer at a time, and the day's own may be busy
        // merging the segments this one rewrites.
        if let Err(e) = self.close(day) {
            return Ok(Err(e));
        }
        let lock = self.overrides.lock.clone();
        let held = lock.hold_all().context("taking the retention lock")?;
        self.refresh()?;
        let cut = self.policy.cut(day, now);
        if !self.needs(day, &cut) {
            return Ok(Ok(false));
        }
        let deleted = self.delete(day, &cut);
        // The counters go with the logs, under the same lock and settings,
        // and on every pass until the cut holds, so a crash or a failure
        // between the deletion and this point leaves no count of a deleted
        // log for long.
        let counted = match &deleted {
            Ok(Deleted::Nothing | Deleted::Marked { .. }) => self.cut_tally(day, &cut),
            Ok(Deleted::Day) | Err(_) => Ok(()),
        };
        if counted.is_err() {
            let cuts = self.uncut.entry(day).or_default();
            if !cuts.contains(&cut) {
                cuts.push(cut.clone());
            }
        }
        // The deletion is committed, so a change saved from here on comes
        // after it.
        drop(held);
        let (kept, wrote) = match deleted {
            Err(e) => return Ok(Err(e)),
            Ok(Deleted::Day) => return Ok(Ok(true)),
            Ok(Deleted::Nothing) => (Ok(true), false),
            Ok(Deleted::Marked {
                index,
                writer,
                reader,
            }) => (self.compact(day, &index, *writer, &reader), true),
        };
        Ok(match (kept, counted) {
            (Err(e), _) | (Ok(true), Err(e)) => Err(e),
            // The day went whole, its counters with it.
            (Ok(false), _) => Ok(true),
            (Ok(true), Ok(())) => {
                self.swept.insert(day, cut);
                Ok(wrote)
            }
        })
    }

    /// Marks what `cut` takes from `day` deleted and commits that, or
    /// deletes the whole day when `cut` keeps nothing. A day with nothing
    /// left to mark that still holds deleted logs, from a pass that stopped
    /// before its merge, is handed on to be merged.
    fn delete(&mut self, day: Date, cut: &Cut) -> Result<Deleted> {
        if matches!(cut, Cut::AllBut(kept) if kept.is_empty()) {
            self.trash(day)?;
            return Ok(Deleted::Day);
        }
        let dir = self.root.join(day.to_string());
        // The pass saw a real directory, but it may have been swapped since.
        anyhow::ensure!(real_dir(&dir)?, "{} is gone", dir.display());
        let index = Index::open_in_dir(&dir)?;
        let reader: IndexReader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        let query = cut.query(&self.fields);
        let marked = reader.searcher().search(&*query, &Count)? > 0;
        let metas = index.searchable_segment_metas()?;
        if !marked && !metas.iter().any(tantivy::index::SegmentMeta::has_deletes) {
            return Ok(Deleted::Nothing);
        }
        let mut writer: IndexWriter = index.writer_with_num_threads(1, WRITER_HEAP)?;
        writer.set_merge_policy(Box::new(NoMergePolicy));
        if marked {
            writer.delete_query(query)?;
            writer.commit()?;
        }
        Ok(Deleted::Marked {
            index,
            writer: Box::new(writer),
            reader,
        })
    }

    /// Rewrites the segments of `day` that hold deleted logs, so the logs
    /// leave the disk, and deletes the day when nothing is left. Returns
    /// whether the day is kept.
    fn compact(
        &mut self,
        day: Date,
        index: &Index,
        mut writer: IndexWriter,
        reader: &IndexReader,
    ) -> Result<bool> {
        let deleted: Vec<SegmentId> = index
            .searchable_segment_metas()?
            .iter()
            .filter(|m| m.has_deletes())
            .map(tantivy::index::SegmentMeta::id)
            .collect();
        if !deleted.is_empty() {
            writer.merge(&deleted).wait()?;
        }
        writer.wait_merging_threads()?;
        reader.reload()?;
        if reader.searcher().num_docs() > 0 {
            return Ok(true);
        }
        self.trash(day).map(|()| false)
    }

    /// Removes what a crash left in `.trash` between renaming a day there
    /// and removing it: real directories with the names [`State::trash`]
    /// gives them. Anything else stays and is reported, and so does a
    /// `.trash` that is not a real directory.
    fn empty_trash(&self) {
        let trash = self.root.join(".trash");
        match real_dir(&trash) {
            Ok(true) => {}
            Ok(false) => return,
            Err(e) => return report_trash(&e),
        }
        let Ok(entries) = std::fs::read_dir(&trash) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let dir = entry.file_type().is_ok_and(|t| t.is_dir());
            if !dir || !trashed(&entry.file_name().to_string_lossy()) {
                report_trash(&anyhow::anyhow!(
                    "{} is not Davy's, left alone",
                    path.display()
                ));
            } else if let Err(e) = std::fs::remove_dir_all(&path) {
                report_trash(&e.into());
            }
        }
    }

    /// Deletes the whole of `day`: renamed into `.trash` first, so a search
    /// sees all of it or none.
    fn trash(&mut self, day: Date) -> Result<()> {
        self.drop_tally(day);
        self.close(day)?;
        self.swept.remove(&day);
        self.uncut.remove(&day);
        days::forget(&self.root, day);
        let trash = self.root.join(".trash");
        std::fs::create_dir_all(&trash)?;
        anyhow::ensure!(real_dir(&trash)?, "{} is gone", trash.display());
        let dir = self.root.join(day.to_string());
        anyhow::ensure!(real_dir(&dir)?, "{} is gone", dir.display());
        let target = trash.join(format!("{day}-{}", now_ns()));
        std::fs::rename(&dir, &target)?;
        std::fs::remove_dir_all(&target)?;
        Ok(())
    }
}

#[expect(
    clippy::print_stderr,
    reason = "the server has no logger yet, stderr is its log"
)]
fn report(day: Date, e: &anyhow::Error) {
    eprintln!("log writer: retention of {day}, skipped until the next pass: {e:#}");
}

#[expect(
    clippy::print_stderr,
    reason = "the server has no logger yet, stderr is its log"
)]
fn report_stray(root: &std::path::Path, stray: &Stray) {
    eprintln!("{}", stray_line(root, stray));
}

/// The line that reports `stray`. The path is quoted and escaped, so a
/// name with a newline or a terminal escape cannot forge a line of the log.
#[expect(
    clippy::unnecessary_debug_formatting,
    reason = "the escaping of Debug is the point"
)]
fn stray_line(root: &std::path::Path, stray: &Stray) -> String {
    format!(
        "log writer: {:?} {}, so it is not searched, written or deleted until it is moved away",
        root.join(&stray.name),
        stray.why
    )
}

#[expect(
    clippy::print_stderr,
    reason = "the server has no logger yet, stderr is its log"
)]
fn report_entry(e: &anyhow::Error) {
    eprintln!("log writer: retention skipped an entry it cannot read: {e:#}");
}

#[expect(
    clippy::print_stderr,
    reason = "the server has no logger yet, stderr is its log"
)]
fn report_trash(e: &anyhow::Error) {
    eprintln!("log writer: emptying the trash: {e:#}");
}

/// Whether `name` is one [`State::trash`] gives a day: `YYYY-MM-DD-` and
/// digits.
fn trashed(name: &str) -> bool {
    name.split_at_checked(10).is_some_and(|(day, rest)| {
        let stamp = rest.strip_prefix('-').unwrap_or_default();
        parse_day(day).is_some() && !stamp.is_empty() && stamp.bytes().all(|b| b.is_ascii_digit())
    })
}

#[cfg(test)]
#[path = "retention_tests.rs"]
mod tests;
