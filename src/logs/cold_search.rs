//! Reading cold days for a search, a count or a histogram.
//!
//! A day is read as its hot index, if it has one, and its segments, except
//! that a hot day sealed for a segment that exists is left out: that
//! segment holds the same logs. The segments are listed first and again
//! once the day is read. A day that turned cold in between, or lost a
//! segment to retention, makes the pass start over, as a day deleted while
//! it is read does: so no log is read twice or missed.
//!
//! Blocks are read newest first, as their headers order them, and a search
//! stops once no block left can hold a log newer than those it has. A scan
//! has a time budget. When it runs out, what was found is returned with
//! the part of the window not read, and a search drops what it found in
//! that part, so a page that follows misses nothing.
//!
//! A count takes a block whose rows all fall in one bucket and all match
//! from its header, after checking its checksum, so a corrupt block is
//! left out of a count as it is out of a search.

use super::{
    super::{LogQuery, days, hit::Row},
    file::ColdFile,
    format::Header,
    layout,
    query::Matcher,
};
use anyhow::{Result, bail};
use jiff::civil::Date;
use serde::Serialize;
use std::{
    path::Path,
    time::{Duration, Instant},
};

/// How long a search reads cold files at most, unless asked otherwise.
pub const BUDGET: Duration = Duration::from_secs(5);

/// What a pass read of the cold tier. Absent when it read no cold file.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ColdScan {
    /// The first and last nanosecond of the window that lie in the cold
    /// days read.
    pub from_ns: i64,
    pub to_ns: i64,
    /// How long reading cold files took.
    pub took_ms: u64,
    /// Rows decompressed and checked.
    pub rows: u64,
    /// When the budget ran out: the part of the window from its start to
    /// here was not wholly read.
    pub unscanned_to_ns: Option<i64>,
    /// Files, blocks and their rows left out because a check failed. Each
    /// is reported as a warning.
    pub corrupt_files: u64,
    pub corrupt_blocks: u64,
    pub corrupt_rows: u64,
}

/// Text a pass fails with when a day changed tier while it was read, so
/// that `retrying` runs it again.
pub(in crate::logs::store) const CHANGED: &str = "a day changed tier while it was read";

/// The days with logs from `since` to `until`, hot, cold or both, newest
/// first.
pub(in crate::logs::store) fn days(root: &Path, since: i64, until: i64) -> Result<Vec<Date>> {
    let mut all = days::current(root)?;
    all.extend(layout::days(root)?);
    all.sort_unstable_by(|a, b| b.cmp(a));
    all.dedup();
    let (first, last) = (crate::time::day_of(since), crate::time::day_of(until));
    all.retain(|d| (first..=last).contains(d));
    Ok(all)
}

/// What of one day a pass reads.
pub(in crate::logs::store) struct Parts {
    day: Date,
    /// Whether to read the hot index.
    pub(in crate::logs::store) hot: bool,
    /// The segments to read, as listed before the hot day was looked at.
    pub(in crate::logs::store) segments: Vec<u32>,
}

impl Parts {
    pub(in crate::logs::store) fn of(root: &Path, day: Date) -> Result<Self> {
        let segments = layout::segments(root, day)?;
        let seal = layout::seal(&root.join(day.to_string()))?;
        Ok(Self {
            day,
            hot: seal.is_none_or(|s| !segments.contains(&s)),
            segments,
        })
    }

    /// The directories of the segments to read, newest first.
    pub(in crate::logs::store) fn segment_dirs(&self, root: &Path) -> Vec<std::path::PathBuf> {
        let dirs = self.segments.iter().rev();
        dirs.map(|s| layout::segment_dir(root, self.day, *s))
            .collect()
    }

    /// Fails with [`CHANGED`] when the day's segments are not those read.
    pub(in crate::logs::store) fn unchanged(&self, root: &Path) -> Result<()> {
        if layout::segments(root, self.day)? != self.segments {
            bail!(CHANGED);
        }
        Ok(())
    }
}

/// One file of a day, open, with what matches in it.
struct Open {
    file: ColdFile,
    project: u64,
    matcher: usize,
}

/// A block a pass may read: its file, its number and its header.
#[derive(Clone, Copy)]
struct Candidate {
    file: usize,
    block: usize,
    header: Header,
}

/// A cold pass over the days of one query.
pub(in crate::logs::store) struct Scan<'q> {
    q: &'q LogQuery,
    budget: Duration,
    started: Option<Instant>,
    /// One matcher for each kind of day: with or without `values` and
    /// `pattern`.
    matchers: Vec<((bool, bool), Matcher)>,
    info: ColdScan,
}

impl<'q> Scan<'q> {
    pub(in crate::logs::store) fn new(q: &'q LogQuery, budget: Duration) -> Self {
        Self {
            q,
            budget,
            started: None,
            matchers: Vec::new(),
            info: ColdScan::default(),
        }
    }

    /// Whether the budget ran out.
    pub(in crate::logs::store) fn out_of_time(&self) -> bool {
        self.info.unscanned_to_ns.is_some()
    }

    /// Where the budget ran out: from the window's start to here, not all
    /// was read.
    pub(in crate::logs::store) fn unscanned_to(&self) -> Option<i64> {
        self.info.unscanned_to_ns
    }

    /// What the pass read, none if it opened no cold file.
    pub(in crate::logs::store) fn finish(mut self) -> Option<ColdScan> {
        let started = self.started?;
        self.info.took_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        Some(self.info)
    }

    /// The newest `need` logs of `parts`' segments that match, newest
    /// first, with their projects. Fewer when the budget runs out.
    pub(in crate::logs::store) fn top(
        &mut self,
        root: &Path,
        parts: &Parts,
        need: usize,
    ) -> Result<Vec<(u64, Row)>> {
        let mut found: Vec<(u64, Row)> = Vec::new();
        if need == 0 || parts.segments.is_empty() {
            return Ok(found);
        }
        let (mut files, mut candidates) = self.open(root, parts)?;
        candidates.sort_by_key(|c| std::cmp::Reverse(c.header.max_ts));
        for c in candidates {
            if found.len() >= need && c.header.max_ts < found[need - 1].1.ts {
                break;
            }
            let Some(rows) = self.read(&mut files, c)? else {
                break;
            };
            let project = files[c.file].project;
            found.extend(rows.into_iter().map(|r| (project, r)));
            found.sort_by_key(|f| std::cmp::Reverse(f.1.ts));
            found.truncate(need);
        }
        Ok(found)
    }

    /// Adds the logs of `parts`' segments that match to `counts`, in the
    /// buckets of `step` nanoseconds from `from`.
    pub(in crate::logs::store) fn count(
        &mut self,
        root: &Path,
        parts: &Parts,
        (from, step): (i64, i64),
        counts: &mut [u64],
    ) -> Result<()> {
        if parts.segments.is_empty() {
            return Ok(());
        }
        let bucket = |ts: i64| {
            let b = ts.checked_sub(from)?.checked_div(step)?;
            usize::try_from(b).ok()
        };
        let (mut files, mut candidates) = self.open(root, parts)?;
        candidates.sort_by_key(|c| std::cmp::Reverse(c.header.max_ts));
        for c in candidates {
            let h = c.header;
            let whole = bucket(h.min_ts).filter(|b| bucket(h.max_ts) == Some(*b));
            let matcher = &self.matchers[files[c.file].matcher].1;
            if let Some(b) = whole.filter(|_| matcher.counts_whole(&h)) {
                if self.spent(c) {
                    break;
                }
                let open = &files[c.file];
                if let Err(e) = open.file.checked(c.block) {
                    self.left_out(open.project, c, &e);
                } else if let Some(n) = counts.get_mut(b) {
                    *n += u64::from(h.rows);
                }
                continue;
            }
            let Some(rows) = self.read(&mut files, c)? else {
                break;
            };
            for b in rows.iter().filter_map(|r| bucket(r.ts)) {
                if let Some(n) = counts.get_mut(b) {
                    *n += 1;
                }
            }
        }
        Ok(())
    }

    /// Opens the files of `parts`' segments a query may match, and lists
    /// the blocks that may match.
    fn open(&mut self, root: &Path, parts: &Parts) -> Result<(Vec<Open>, Vec<Candidate>)> {
        if self.started.is_none() {
            self.started = Some(Instant::now());
            (self.info.from_ns, self.info.to_ns) = (i64::MAX, i64::MIN);
        }
        let (first, last) = window(parts.day, self.q);
        self.info.from_ns = self.info.from_ns.min(first);
        self.info.to_ns = self.info.to_ns.max(last);
        let mut files = Vec::new();
        let mut candidates = Vec::new();
        for seq in &parts.segments {
            let dir = layout::segment_dir(root, parts.day, *seq);
            for (project, path) in layout::files(&dir)? {
                if self.q.project.is_some_and(|p| p.cast_unsigned() != project) {
                    continue;
                }
                let opened = ColdFile::open(&path);
                let file = match opened.and_then(|f| f.belongs(parts.day, project).map(|()| f)) {
                    Ok(f) => f,
                    Err(e) if missing(&e) => return Err(e),
                    Err(e) => {
                        self.info.corrupt_files += 1;
                        corrupt(format_args!("{}: {e:#}", path.display()));
                        continue;
                    }
                };
                let matcher = self.matcher(file.footer.meta.values, file.footer.meta.pattern)?;
                let m = &self.matchers[matcher].1;
                for (block, e) in file.footer.blocks.iter().enumerate() {
                    let h = e.header;
                    if m.may_match(project, h.min_ts, h.max_ts, h.levels) {
                        let file = files.len();
                        candidates.push(Candidate {
                            file,
                            block,
                            header: h,
                        });
                    }
                }
                files.push(Open {
                    file,
                    project,
                    matcher,
                });
            }
        }
        Ok((files, candidates))
    }

    fn matcher(&mut self, values: bool, pattern: bool) -> Result<usize> {
        if let Some(i) = self
            .matchers
            .iter()
            .position(|(k, _)| *k == (values, pattern))
        {
            return Ok(i);
        }
        self.matchers
            .push(((values, pattern), Matcher::new(self.q, values, pattern)?));
        Ok(self.matchers.len() - 1)
    }

    /// The rows of block `c` that match. `None` once the budget ran out;
    /// an empty list for a block that fails its checks, reported.
    fn read(&mut self, files: &mut [Open], c: Candidate) -> Result<Option<Vec<Row>>> {
        if self.spent(c) {
            return Ok(None);
        }
        let open = &mut files[c.file];
        let block = match open.file.block(c.block) {
            Ok(b) => b,
            Err(e) => {
                let project = open.project;
                self.left_out(project, c, &e);
                return Ok(Some(Vec::new()));
            }
        };
        self.info.rows += block.len() as u64;
        let matcher = &self.matchers[open.matcher].1;
        let mut rows = Vec::new();
        for i in matcher.rows(open.project, &block)? {
            rows.push(block.row(i)?);
        }
        Ok(Some(rows))
    }

    /// Whether the budget ran out before block `c`, which is then where
    /// the pass stops.
    fn spent(&mut self, c: Candidate) -> bool {
        if self.started.is_some_and(|s| s.elapsed() > self.budget) || blocks_spent() {
            self.info.unscanned_to_ns = Some(c.header.max_ts.min(self.q.until));
            return true;
        }
        false
    }

    /// Counts block `c`, which failed a check, and reports it.
    fn left_out(&mut self, project: u64, c: Candidate, e: &anyhow::Error) {
        self.info.corrupt_blocks += 1;
        self.info.corrupt_rows += u64::from(c.header.rows);
        corrupt(format_args!("project {project}, {e:#}"));
    }
}

#[cfg(test)]
thread_local! {
    /// How many blocks a pass on this thread takes before its budget runs
    /// out, for a test that needs the cut at a set block.
    pub(in crate::logs::store) static BLOCKS_LEFT: std::cell::Cell<Option<usize>> =
        const { std::cell::Cell::new(None) };
}

/// Whether a test's [`BLOCKS_LEFT`] ran out; never outside tests.
fn blocks_spent() -> bool {
    #[cfg(test)]
    if let Some(left) = BLOCKS_LEFT.get() {
        BLOCKS_LEFT.set(Some(left.saturating_sub(1)));
        return left == 0;
    }
    false
}

/// The first and last nanosecond of `day` inside `q`'s window.
fn window(day: Date, q: &LogQuery) -> (i64, i64) {
    let start = crate::time::midnight(day);
    let end = day
        .tomorrow()
        .map_or(i64::MAX, |d| crate::time::midnight(d) - 1);
    (start.max(q.since), end.min(q.until))
}

/// Whether `e` says a file is gone, as retention leaves it.
fn missing(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        c.downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound)
    })
}

fn corrupt(what: std::fmt::Arguments<'_>) {
    crate::self_report::warn(
        "log search",
        format_args!("a cold file is corrupt, left out: {what}"),
    );
}

#[cfg(test)]
#[path = "cold_search_tests.rs"]
mod tests;
