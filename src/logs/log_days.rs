//! Raw bytes of the logs stored per index day and project, and the pairs
//! retention cut, counted by the log writer for `log_days`
//! (`docs/design.md`, "What is counted: stored logs, cut pairs, SQLite
//! size"). A count is pending until the commit of its day holds. Settled
//! counts and cut marks wait in [`LogDays`], in the order they happened,
//! until the usage flusher sends them to the SQLite writer.

use super::{State, retention::Cut};
use crate::usage::Count;
use jiff::civil::Date;
use std::{
    collections::{BTreeSet, HashMap, VecDeque},
    sync::{Arc, Mutex, PoisonError},
};

/// One day of one project's stored logs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub day: Date,
    pub project: i64,
    pub count: Count,
}

/// Rows of `log_days` whose logs retention deleted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mark {
    /// The rows of these projects on `day`.
    Only(Date, Vec<i64>),
    /// Every row of `day` but those of these projects: all of them when
    /// the list is empty.
    AllBut(Date, Vec<i64>),
    /// Every row of a day not in the set: the days and strays the log
    /// directory held when it was listed.
    Absent(BTreeSet<Date>),
}

impl Mark {
    /// The mark of what `cut` deleted from `day`.
    pub(super) fn of(day: Date, cut: &Cut) -> Self {
        let ids = |ps: &BTreeSet<u64>| ps.iter().map(|p| p.cast_signed()).collect();
        match cut {
            Cut::Only(ps) => Self::Only(day, ids(ps)),
            Cut::AllBut(ps) => Self::AllBut(day, ids(ps)),
        }
    }
}

/// Counts, then the marks queued after them. The writer applies the
/// counts first, so a mark never lands before a count that came before it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Segment {
    pub rows: Vec<Row>,
    pub marks: Vec<Mark>,
}

/// What one job takes to the writer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Flush {
    /// When this server's log writer started counting, in ms, until a
    /// job that carries it is committed.
    pub start: Option<i64>,
    pub segments: Vec<Segment>,
}

type Counts = HashMap<(Date, i64), Count>;

#[derive(Default)]
struct Queue {
    start: Option<i64>,
    segments: VecDeque<(Counts, Vec<Mark>)>,
}

/// Settled counts and marks until the next flush, shared by the log writer
/// and the usage flusher like [`crate::usage::Counter`]. Keys are an index
/// day and a project, so the map stays small.
#[derive(Clone, Default)]
pub struct LogDays(Arc<Mutex<Queue>>);

impl LogDays {
    /// Counts that started at `start_ms`.
    pub fn new(start_ms: i64) -> Self {
        let queue = Queue {
            start: Some(start_ms),
            segments: VecDeque::new(),
        };
        Self(Arc::new(Mutex::new(queue)))
    }

    fn queue(&self) -> std::sync::MutexGuard<'_, Queue> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Adds counts whose commit held.
    fn add(&self, day: Date, counts: HashMap<i64, Count>) {
        let mut q = self.queue();
        if q.segments.back().is_none_or(|(_, marks)| !marks.is_empty()) {
            q.segments.push_back(Default::default());
        }
        let (last, _) = q.segments.back_mut().expect("pushed above");
        for (project, count) in counts {
            last.entry((day, project)).or_default().add(count);
        }
    }

    /// Queues `mark` after every count added so far.
    pub(super) fn mark(&self, mark: Mark) {
        let mut q = self.queue();
        match q.segments.back_mut() {
            Some((_, marks)) => marks.push(mark),
            None => q.segments.push_back((Counts::new(), vec![mark])),
        }
    }

    /// Takes everything out, for tests and the last flush.
    pub fn take(&self) -> Flush {
        self.take_up_to(usize::MAX).unwrap_or_default()
    }

    /// Takes the oldest counts and marks out, about `n` rows of them, or
    /// `None` when there is nothing to send. Counts of a segment that do
    /// not fit stay with its marks, so the marks go with the last of them.
    pub fn take_up_to(&self, n: usize) -> Option<Flush> {
        let mut q = self.queue();
        let mut flush = Flush {
            start: q.start.take(),
            segments: Vec::new(),
        };
        let mut room = n.max(1);
        while room > 0
            && let Some((counts, marks)) = q.segments.pop_front()
        {
            let mut rows = sorted(counts);
            if rows.len() > room {
                let rest = rows.split_off(room);
                let rest = rest.into_iter().map(|r| ((r.day, r.project), r.count));
                q.segments.push_front((rest.collect(), marks));
                flush.segments.push(Segment {
                    rows,
                    marks: Vec::new(),
                });
                break;
            }
            room = room.saturating_sub(rows.len() + marks.len());
            flush.segments.push(Segment { rows, marks });
        }
        (flush.start.is_some() || !flush.segments.is_empty()).then_some(flush)
    }

    /// Puts back what a job the writer did not commit took, before
    /// anything queued since.
    pub fn put_back(&self, flush: Flush) {
        let mut q = self.queue();
        if flush.start.is_some() {
            q.start = flush.start;
        }
        for s in flush.segments.into_iter().rev() {
            let counts = s.rows.into_iter().map(|r| ((r.day, r.project), r.count));
            q.segments.push_front((counts.collect(), s.marks));
        }
    }
}

fn sorted(counts: Counts) -> Vec<Row> {
    let mut rows: Vec<Row> = counts
        .into_iter()
        .map(|((day, project), count)| Row {
            day,
            project,
            count,
        })
        .collect();
    rows.sort_by_key(|r| (r.day, r.project));
    rows
}

impl State {
    /// Counts a record of `project` with `bytes` in `day` as pending.
    pub(super) fn count_stored(&mut self, day: Date, project: i64, bytes: u64) {
        let pending = self.pending.entry(day).or_default();
        pending
            .entry(project)
            .or_default()
            .add(Count { records: 1, bytes });
    }

    /// The commit of `day` held: its pending counts settle, those of the
    /// pattern counters and of `log_days`.
    pub(super) fn settled(&mut self, day: Date) {
        self.settle_tally(day);
        if let Some(counts) = self.pending.remove(&day) {
            self.log_days.add(day, counts);
        }
    }

    /// The pending counts of `day` are lost with its logs.
    pub(super) fn lost(&mut self, day: Date) {
        self.discard_tally(day);
        self.pending.remove(&day);
    }

    /// Marks what `cut` deleted from `day` cut. Called under the retention
    /// lock, with the cut the deletion used.
    pub(super) fn mark_cut(&self, day: Date, cut: &Cut) {
        self.log_days.mark(Mark::of(day, cut));
    }

    /// Marks every row of `day`, which is gone.
    pub(super) fn mark_gone(&self, day: Date) {
        self.log_days.mark(Mark::AllBut(day, Vec::new()));
    }

    /// Marks every row of a day not in `present`, once per server. A log
    /// directory that holds no day marks nothing: a restore that brought
    /// SQLite back first or a mount that is missing looks the same, and a
    /// mark is never taken back. The first start that finds a day marks
    /// the rest. A fresh data directory has no row to mark anyway.
    pub(super) fn mark_absent(&mut self, present: BTreeSet<Date>) {
        if !self.listed {
            self.listed = true;
            if !present.is_empty() {
                self.log_days.mark(Mark::Absent(present));
            }
        }
    }
}

#[cfg(test)]
#[path = "log_days_tests.rs"]
mod tests;
