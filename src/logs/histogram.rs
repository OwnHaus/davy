//! Counts of the logs a query matches over time, for the histogram above
//! the log list.

use super::{DAY_NS, LogQuery, days, retrying, schema};
use anyhow::Result;
use std::path::Path;
use tantivy::{
    DocId, Score, SegmentReader,
    collector::{Collector, SegmentCollector},
    columnar::Column,
};

/// Buckets a histogram has at most.
pub const MAX_BUCKETS: usize = 200;

const MS: i64 = 1_000_000;
const SEC: i64 = 1000 * MS;
const MIN: i64 = 60 * SEC;
const HOUR: i64 = 60 * MIN;

/// The widths a bucket may have: round numbers of milliseconds, seconds,
/// minutes, hours and days, so bucket edges fall on round times.
const STEPS: [i64; 30] = [
    MS,
    2 * MS,
    5 * MS,
    10 * MS,
    20 * MS,
    50 * MS,
    100 * MS,
    200 * MS,
    500 * MS,
    SEC,
    2 * SEC,
    5 * SEC,
    10 * SEC,
    15 * SEC,
    30 * SEC,
    MIN,
    2 * MIN,
    5 * MIN,
    10 * MIN,
    15 * MIN,
    30 * MIN,
    HOUR,
    2 * HOUR,
    3 * HOUR,
    6 * HOUR,
    12 * HOUR,
    DAY_NS,
    2 * DAY_NS,
    7 * DAY_NS,
    14 * DAY_NS,
];

/// How many logs a query matches in each of `counts.len()` buckets of
/// `step` nanoseconds, the first starting at `from`.
#[derive(Debug, PartialEq, Eq)]
pub struct Histogram {
    pub from: i64,
    pub step: i64,
    pub counts: Vec<u64>,
}

/// The first bucket's start and the narrowest step of [`STEPS`] that covers
/// `since..=until` in at most [`MAX_BUCKETS`] buckets starting on a multiple
/// of the step, or whole days past the widest.
fn buckets_for(since: i64, until: i64) -> (i64, i64) {
    let max = i64::try_from(MAX_BUCKETS).unwrap_or(i64::MAX);
    let fits = |step: i64| (until - since.div_euclid(step) * step) / step < max;
    let step = STEPS.into_iter().find(|s| fits(*s)).unwrap_or_else(|| {
        let mut days = (until - since) / DAY_NS / max + 1;
        while !fits(days * DAY_NS) {
            days += 1;
        }
        days * DAY_NS
    });
    (since.div_euclid(step) * step, step)
}

/// What `q` matches from `q.since` to `q.until` in round buckets, at most
/// [`MAX_BUCKETS`]. `q.limit` does not apply.
pub fn histogram(root: &Path, q: &LogQuery) -> Result<Histogram> {
    retrying(|| histogram_once(root, q))
}

fn histogram_once(root: &Path, q: &LogQuery) -> Result<Histogram> {
    anyhow::ensure!(q.since <= q.until, "the histogram starts after it ends");
    let (from, step) = buckets_for(q.since, q.until);
    let buckets = Buckets {
        from,
        step,
        len: usize::try_from((q.until - from) / step + 1)?,
    };
    let f = schema().1;
    let mut counts = vec![0; buckets.len];
    for day in days(root, q.since, q.until)? {
        let day = day?;
        let part = day.searcher.search(&day.matching(&f, q)?, &buckets)?;
        counts.iter_mut().zip(part).for_each(|(c, n)| *c += n);
    }
    Ok(Histogram { from, step, counts })
}

/// Counts the matching logs of each bucket from the `ts` fast field, in
/// whole nanoseconds. tantivy's histogram aggregation works in `f64`, which
/// holds a time of today only to the nearest 256 ns.
struct Buckets {
    from: i64,
    step: i64,
    len: usize,
}

struct SegmentBuckets {
    ts: Column<i64>,
    from: i64,
    step: i64,
    counts: Vec<u64>,
    block: Vec<Option<i64>>,
}

impl Collector for Buckets {
    type Fruit = Vec<u64>;
    type Child = SegmentBuckets;

    fn for_segment(&self, _: u32, segment: &SegmentReader) -> tantivy::Result<SegmentBuckets> {
        Ok(SegmentBuckets {
            ts: segment.fast_fields().i64("ts")?,
            from: self.from,
            step: self.step,
            counts: vec![0; self.len],
            block: Vec::new(),
        })
    }

    fn requires_scoring(&self) -> bool {
        false
    }

    fn merge_fruits(&self, parts: Vec<Vec<u64>>) -> tantivy::Result<Vec<u64>> {
        let mut counts = vec![0; self.len];
        for part in parts {
            counts.iter_mut().zip(part).for_each(|(c, n)| *c += n);
        }
        Ok(counts)
    }
}

impl SegmentBuckets {
    fn count(&mut self, ts: i64) {
        let bucket = usize::try_from((ts - self.from) / self.step).ok();
        if let Some(n) = bucket.and_then(|b| self.counts.get_mut(b)) {
            *n += 1;
        }
    }
}

impl SegmentCollector for SegmentBuckets {
    type Fruit = Vec<u64>;

    fn collect(&mut self, doc: DocId, _: Score) {
        if let Some(ts) = self.ts.first(doc) {
            self.count(ts);
        }
    }

    fn collect_block(&mut self, docs: &[DocId]) {
        let mut block = std::mem::take(&mut self.block);
        block.clear();
        block.resize(docs.len(), None);
        self.ts.first_vals(docs, &mut block);
        for ts in block.iter().flatten() {
            self.count(*ts);
        }
        self.block = block;
    }

    fn harvest(self) -> Vec<u64> {
        self.counts
    }
}

#[cfg(test)]
mod tests {
    use super::{DAY_NS, HOUR, MS, SEC, buckets_for};

    #[test]
    fn a_window_gets_the_narrowest_round_step_of_at_most_200_buckets() {
        let cases = [
            ((0, 50 * MS), (0, MS)),
            ((0, HOUR - 1), (0, 30 * SEC)),
            ((7 * SEC + 1, 407 * SEC - 1), (5 * SEC, 5 * SEC)),
            ((0, 200 * SEC - 1), (0, SEC)),
            ((0, 200 * SEC), (0, 2 * SEC)),
            ((DAY_NS, 31 * DAY_NS), (DAY_NS, 6 * HOUR)),
            ((0, 3000 * DAY_NS), (0, 16 * DAY_NS)),
        ];
        for ((since, until), expected) in cases {
            assert_eq!(buckets_for(since, until), expected, "{since}..={until}");
        }
    }
}
