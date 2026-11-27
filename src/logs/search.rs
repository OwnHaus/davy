//! Search over the days of the log root, hot and cold, newest first.

use super::{
    LogHit, LogQuery,
    cold::{self, ColdScan, Parts, Scan},
    days, hit, schema,
};
use anyhow::Result;
use std::{path::Path, time::Duration};
use tantivy::{DocAddress, Order, TantivyDocument, collector::TopDocs};

/// What a search found, and what it read of the cold tier.
#[derive(Debug, Default)]
pub struct Found {
    /// The newest `limit` hits, oldest first.
    pub hits: Vec<LogHit>,
    pub cold: Option<ColdScan>,
}

/// The newest `q.limit` hits, oldest first.
pub fn search(root: &Path, q: &LogQuery) -> Result<Vec<LogHit>> {
    search_for(root, q, cold::BUDGET).map(|f| f.hits)
}

/// [`search`], reading cold files for `budget` at most, and what it read
/// of them.
pub fn search_for(root: &Path, q: &LogQuery, budget: Duration) -> Result<Found> {
    retrying(|| search_once(root, q, budget))
}

/// [`search`] with no time budget, so a test that compares two reads
/// never compares a read the budget cut.
#[cfg(test)]
pub fn search_all(root: &Path, q: &LogQuery) -> Result<Vec<LogHit>> {
    search_for(root, q, Duration::MAX).map(|f| f.hits)
}

/// Runs `f` a second time when its first pass met a day that retention
/// deleted between the `meta.json` check and the open, or that turned
/// cold while it was read. tantivy reports a deleted file only as text, so
/// a pass that no longer sees the day is the way to recover.
pub(super) fn retrying<T>(f: impl Fn() -> Result<T>) -> Result<T> {
    f().or_else(|e| {
        let missing = format!("{e:#}");
        if missing.contains("does not exist")
            || missing.contains("No such file")
            || missing.contains(cold::CHANGED)
        {
            f()
        } else {
            Err(e)
        }
    })
}

/// A hit of one day before it is read: a document of the hot index or a
/// cold row, with its time.
enum Top {
    Hot(i64, DocAddress),
    Cold(u64, hit::Row),
}

impl Top {
    fn ts(&self) -> i64 {
        match self {
            Self::Hot(ts, _) => *ts,
            Self::Cold(_, row) => row.ts,
        }
    }
}

fn search_once(root: &Path, q: &LogQuery, budget: Duration) -> Result<Found> {
    let f = schema().1;
    let mut hits = Vec::new();
    let mut skip = q.skip;
    let mut scan = Scan::new(q, budget);
    for day in cold::days(root, q.since, q.until)? {
        if hits.len() >= q.limit || scan.out_of_time() {
            break;
        }
        let parts = Parts::of(root, day)?;
        let want = q.limit - hits.len() + skip;
        let hot = match parts.hot.then(|| days::one(root, day)).flatten() {
            Some(d) => Some(d?),
            None => None,
        };
        let mut hot_top = Vec::new();
        if let Some(d) = &hot {
            let top = d.searcher.search(
                &d.matching(&f, q)?,
                &TopDocs::with_limit(want).order_by_fast_field::<i64>("ts", Order::Desc),
            )?;
            hot_top = top
                .into_iter()
                .map(|(ts, a)| Top::Hot(ts.unwrap_or(i64::MIN), a))
                .collect();
        }
        let cold_top = scan.top(root, &parts, want)?;
        parts.unchanged(root)?;
        let cold_top = cold_top.into_iter().map(|(p, r)| Top::Cold(p, r));
        // Past where the budget ran out the logs found are some, not all:
        // they are left for a page that starts there.
        let unscanned = scan.unscanned_to().unwrap_or(i64::MIN);
        for top in merge(hot_top, cold_top) {
            if top.ts() <= unscanned {
                break;
            }
            if skip > 0 && top.ts() == q.until {
                skip -= 1;
                continue;
            }
            skip = 0;
            if hits.len() == q.limit {
                break;
            }
            hits.push(match top {
                Top::Hot(_, address) => {
                    let d = hot.as_ref().expect("a hot hit comes from the hot day");
                    let doc: TantivyDocument = d.searcher.doc(address)?;
                    hit::hit(&d.index.schema(), &f, &doc)?
                }
                Top::Cold(project, row) => hit::to_hit(project, &row)?,
            });
        }
    }
    hits.reverse();
    Ok(Found {
        hits,
        cold: scan.finish(),
    })
}

/// `hot` and `cold`, each newest first, as one list newest first. Of two
/// hits at the same time the hot one comes first.
fn merge(hot: Vec<Top>, cold: impl Iterator<Item = Top>) -> Vec<Top> {
    let mut out = Vec::with_capacity(hot.len());
    let mut cold = cold.peekable();
    for h in hot {
        while let Some(c) = cold.next_if(|c| c.ts() > h.ts()) {
            out.push(c);
        }
        out.push(h);
    }
    out.extend(cold);
    out
}
