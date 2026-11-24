//! Deletes the logs each project keeps no longer.
//!
//! Logs live in one index per UTC day that holds every project. A day older
//! than the longest retention of all is deleted whole, which is all there is
//! to do while no project has a setting of its own. A younger day loses the
//! logs of each project whose retention it outlived: tantivy marks them
//! deleted, and a merge rewrites the segments that held any, so they leave
//! the disk and not only the results. A day left with no logs is deleted.
//!
//! The pass remembers what it took from each day, so a day is read again
//! only when a setting changed or the day outlived another project's
//! retention. A pass that has deleted for longer than its budget stops, and
//! the next one goes on, so writes wait at most about one day's merge.

use super::{DAY_NS, Fields, State, WRITER_HEAP, days, parse_day};
use crate::time::{day_of, now_ns};
use anyhow::Result;
use jiff::civil::Date;
use std::{
    collections::{BTreeSet, HashMap},
    time::{Duration, Instant},
};
use tantivy::{
    Index, IndexWriter, ReloadPolicy, Term,
    collector::Count,
    index::SegmentId,
    indexer::NoMergePolicy,
    query::{AllQuery, BooleanQuery, Occur, Query, TermQuery},
    schema::IndexRecordOption,
};

/// Reads the projects that keep logs for a number of days of their own, by
/// project id. The log writer calls it before each retention pass.
pub type Overrides = Box<dyn FnMut() -> Result<HashMap<u64, u32>> + Send>;

/// Overrides for a server where no project has a setting of its own.
pub fn no_overrides() -> Overrides {
    Box::new(|| Ok(HashMap::new()))
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

impl State {
    /// Deletes what expired at `now`, for at most about `budget` beyond the
    /// first day it deletes from or removes; the rest waits for the next
    /// call. A day that fails is reported and skipped, so one broken day
    /// cannot stop the retention of every day after it.
    pub(super) fn retention(&mut self, now: i64, budget: Duration) -> Result<()> {
        let deadline = Instant::now().checked_add(budget);
        self.empty_trash();
        let mut found = Vec::new();
        for entry in std::fs::read_dir(&self.root)? {
            if let Some(day) = parse_day(&entry?.file_name().to_string_lossy()) {
                found.push(day);
            }
        }
        found.sort_unstable();
        self.swept.retain(|day, _| found.contains(day));
        for day in found {
            match self.sweep_day(day, now) {
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

    /// Deletes what expired at `now` from `day`. Returns whether it wrote
    /// to the disk, which is what costs time.
    fn sweep_day(&mut self, day: Date, now: i64) -> Result<bool> {
        let cut = self.policy.cut(day, now);
        match &cut {
            Cut::AllBut(kept) if kept.is_empty() => self.trash(day)?,
            Cut::Only(gone) if gone.is_empty() => return Ok(false),
            _ if self.swept.get(&day) == Some(&cut) => return Ok(false),
            _ => {
                if self.cut(day, &cut)? {
                    self.swept.insert(day, cut);
                }
            }
        }
        Ok(true)
    }

    /// Removes what a crash left in `.trash` between renaming a day there
    /// and removing it.
    fn empty_trash(&self) {
        let Ok(entries) = std::fs::read_dir(self.root.join(".trash")) else {
            return;
        };
        for entry in entries.flatten() {
            if let Err(e) = std::fs::remove_dir_all(entry.path()) {
                report_trash(&e);
            }
        }
    }

    /// Deletes `cut` from `day`. Returns false when the day is gone after.
    fn cut(&mut self, day: Date, cut: &Cut) -> Result<bool> {
        let index = Index::open_in_dir(self.root.join(day.to_string()))?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        let query = cut.query(&self.fields);
        if reader.searcher().search(&*query, &Count)? == 0 {
            return Ok(true);
        }
        // An index has one writer at a time, and the day's own may be busy
        // merging the segments this one rewrites.
        self.close(day)?;
        let mut writer: IndexWriter = index.writer_with_num_threads(1, WRITER_HEAP)?;
        writer.set_merge_policy(Box::new(NoMergePolicy));
        writer.delete_query(query)?;
        writer.commit()?;
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
        self.trash(day)?;
        Ok(false)
    }

    /// Deletes the whole of `day`: renamed into `.trash` first, so a search
    /// sees all of it or none.
    fn trash(&mut self, day: Date) -> Result<()> {
        self.close(day)?;
        self.swept.remove(&day);
        days::forget(&self.root, day);
        let trash = self.root.join(".trash");
        std::fs::create_dir_all(&trash)?;
        let target = trash.join(format!("{day}-{}", now_ns()));
        std::fs::rename(self.root.join(day.to_string()), &target)?;
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
fn report_trash(e: &std::io::Error) {
    eprintln!("log writer: emptying the trash: {e}");
}

#[cfg(test)]
#[path = "retention_tests.rs"]
mod tests;
