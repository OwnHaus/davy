//! The top log patterns of a window, from the counters in [`super::tally`].

use super::{
    cold::{self, Parts},
    day_dirs, retrying,
    tally::{self, DayTally, Levels, level_index},
};
use crate::{
    logs::{
        LogLevel,
        patterns::{self, OTHER},
    },
    text::size,
    time::day_of,
};
use anyhow::Result;
use jiff::civil::Date;
use serde::Serialize;
use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
    sync::PoisonError,
};

/// Rows a report holds at most.
pub const MAX_PATTERN_ROWS: usize = 100;
/// Services a row names. The rest are summed in one more entry.
const SERVICES: usize = 3;
/// Days a report reads at most, the newest first.
pub const MAX_REPORT_DAYS: usize = 92;
/// Patterns and pairs of pattern and service a report merges at most: it
/// reads no further day once it holds this many.
pub const MAX_MERGED: usize = 50_000;

#[derive(Debug, Clone)]
pub struct PatternQuery {
    pub project: Option<i64>,
    pub since: i64,
    pub until: i64,
    pub min_level: Option<LogLevel>,
    pub limit: usize,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct PatternReport {
    /// The first UTC day counted. Counters are per day, so the report covers
    /// whole days from the day of `since` to the day of `until`, or from a
    /// later day when it is `truncated`.
    pub from: String,
    pub to: String,
    pub records: u64,
    pub bytes: String,
    /// [`Self::bytes`] in bytes, for the savings view.
    #[serde(skip)]
    pub raw_bytes: u64,
    /// Patterns in the window, shown or not.
    pub patterns: usize,
    /// The patterns with the most bytes first.
    pub top: Vec<PatternRow>,
    /// Days with counters that could not be read, left out of the report.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unread: Vec<String>,
    /// Days whose counters started again from nothing that day, because a
    /// file of them could not be read, so they count fewer logs than the
    /// day holds.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub restarted: Vec<String>,
    /// The window held more than a report reads, [`MAX_REPORT_DAYS`] days
    /// or [`MAX_MERGED`] patterns and pairs, so the days before `from` are
    /// left out.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct PatternRow {
    pub id: String,
    pub template: String,
    pub example: String,
    pub records: u64,
    pub bytes: String,
    /// [`Self::bytes`] in bytes, for the savings view.
    #[serde(skip)]
    pub raw_bytes: u64,
    /// Of the bytes of the window, in percent.
    pub share: String,
    /// Records by level, the levels with none left out.
    pub levels: BTreeMap<&'static str, u64>,
    /// Records by service, most first, `null` for records without one.
    pub services: Vec<(Option<String>, u64)>,
}

#[derive(Default)]
struct Sum {
    template: String,
    example: String,
    records: u64,
    bytes: u64,
    levels: [u64; 6],
    services: HashMap<Box<str>, u64>,
}

/// The `q.limit` patterns with the most bytes from `q.since` to `q.until`.
/// A day that turns cold while it is read starts the report over, once,
/// as it does a search.
pub fn top_patterns(root: &Path, q: &PatternQuery) -> Result<PatternReport> {
    retrying(|| top_patterns_once(root, q))
}

fn top_patterns_once(root: &Path, q: &PatternQuery) -> Result<PatternReport> {
    let (first, last) = (day_of(q.since), day_of(q.until));
    // A symlink named like a day is not one, so a report reads no counters
    // from outside the root.
    let days = cold::days(root, q.since, q.until)?;
    let live = tally::live(root);
    let project = q.project.map(i64::cast_unsigned);
    let (mut sums, mut unread, mut restarted) = (HashMap::new(), Vec::new(), Vec::new());
    let mut merged = 0;
    let (mut from, mut truncated) = (first, false);
    for (n, day) in days.into_iter().enumerate() {
        if n == MAX_REPORT_DAYS || merged >= MAX_MERGED {
            truncated = true;
            break;
        }
        from = day;
        let Ok(parts) = Parts::of(root, day) else {
            unread.push(day.to_string());
            continue;
        };
        let held = live.as_ref().and_then(|live| {
            let held = live.lock().unwrap_or_else(PoisonError::into_inner);
            let tally = held.get(&day).filter(|_| parts.hot)?;
            add(&mut sums, &mut merged, tally, q);
            Some(tally.restarted(project))
        });
        if held == Some(true) {
            restarted.push(day.to_string());
        }
        let hot = root.join(day.to_string());
        let mut dirs = parts.segment_dirs(root);
        if held.is_none() && parts.hot && day_dirs::real_dir(&hot).unwrap_or(false) {
            dirs.insert(0, hot);
        }
        // A day that turned cold has its counters in each segment, and
        // late logs in the hot day: their sum counts the day.
        for dir in dirs {
            match DayTally::load(&dir, project) {
                Ok((tally, failed)) => {
                    add(&mut sums, &mut merged, &tally, q);
                    if !failed.is_empty() {
                        unread.push(day.to_string());
                    }
                    if tally.restarted(project) {
                        restarted.push(day.to_string());
                    }
                }
                Err(_) => unread.push(day.to_string()),
            }
        }
        if let Err(e) = parts.unchanged(root) {
            if format!("{e:#}").contains(cold::CHANGED) {
                return Err(e);
            }
            unread.push(day.to_string());
        }
    }
    unread.dedup();
    restarted.dedup();
    let mut report = report(sums, if truncated { from } else { first }, last, q.limit);
    report.unread = unread;
    report.restarted = restarted;
    report.truncated = truncated;
    Ok(report)
}

/// Adds the counters of `tally` to `sums`, and the patterns and pairs it
/// adds to `merged`.
fn add(sums: &mut HashMap<u64, Sum>, merged: &mut usize, tally: &DayTally, q: &PatternQuery) {
    let projects = tally
        .projects
        .iter()
        .filter(|(p, _)| q.project.is_none_or(|want| want.cast_unsigned() == **p));
    let lowest = q.min_level.map_or(0, level_index);
    for (id, entry) in projects.flat_map(|(_, day)| &day.patterns) {
        let services: Vec<_> = entry
            .services
            .iter()
            .map(|(service, levels)| (service, levels, counted(levels, lowest)))
            .filter(|(_, _, (records, _))| *records > 0)
            .collect();
        if services.is_empty() {
            continue;
        }
        let s = sums.entry(*id).or_insert_with(|| {
            *merged += 1;
            Sum::default()
        });
        // The newest day comes first and names the pattern. A line of
        // whitespace alone has the empty template, so records tell.
        if s.records == 0 {
            s.template.clone_from(&entry.template);
            s.example.clone_from(&entry.example);
        }
        for (service, levels, (records, bytes)) in services {
            s.records = s.records.saturating_add(records);
            s.bytes = s.bytes.saturating_add(bytes);
            for (i, [n, _]) in levels.iter().enumerate().skip(lowest) {
                s.levels[i] = s.levels[i].saturating_add(*n);
            }
            let by_service = s.services.entry(service.clone()).or_insert_with(|| {
                *merged += 1;
                0
            });
            *by_service = by_service.saturating_add(records);
        }
    }
}

fn counted(levels: &Levels, lowest: usize) -> (u64, u64) {
    levels[lowest..].iter().fold((0, 0), |(r, b), [n, bytes]| {
        (r.saturating_add(*n), b.saturating_add(*bytes))
    })
}

fn report(sums: HashMap<u64, Sum>, first: Date, last: Date, limit: usize) -> PatternReport {
    let records = sums
        .values()
        .map(|s| s.records)
        .fold(0, u64::saturating_add);
    let bytes = sums.values().map(|s| s.bytes).fold(0, u64::saturating_add);
    let patterns = sums.len();
    let mut rows: Vec<(u64, Sum)> = sums.into_iter().collect();
    rows.sort_unstable_by(|(a_id, a), (b_id, b)| {
        (b.bytes, b.records, a_id).cmp(&(a.bytes, a.records, b_id))
    });
    rows.truncate(limit.min(MAX_PATTERN_ROWS));
    PatternReport {
        from: first.to_string(),
        to: last.to_string(),
        records,
        bytes: size(bytes),
        raw_bytes: bytes,
        patterns,
        top: rows.into_iter().map(|(id, s)| row(id, s, bytes)).collect(),
        unread: Vec::new(),
        restarted: Vec::new(),
        truncated: false,
    }
}

fn row(id: u64, s: Sum, total: u64) -> PatternRow {
    #[expect(
        clippy::cast_precision_loss,
        reason = "a share is shown to one decimal"
    )]
    let share = if total == 0 {
        0.0
    } else {
        s.bytes as f64 * 100.0 / total as f64
    };
    let levels = (1..=6)
        .map(LogLevel::from_code)
        .zip(s.levels)
        .filter(|(_, n)| *n > 0)
        .map(|(level, n)| (level.as_str(), n))
        .collect();
    let template = if id == OTHER {
        format!(
            "(patterns without a count of their own: past {} of a project in a day \
             until seen {} times, past {} always)",
            tally::FREE_PATTERNS,
            tally::ADMIT_AFTER,
            tally::MAX_PATTERNS
        )
    } else {
        s.template
    };
    PatternRow {
        id: patterns::show(id),
        template,
        example: s.example,
        records: s.records,
        bytes: size(s.bytes),
        raw_bytes: s.bytes,
        share: format!("{share:.1}%"),
        levels,
        services: services(s.services),
    }
}

fn services(by_name: HashMap<Box<str>, u64>) -> Vec<(Option<String>, u64)> {
    let mut all: Vec<(Box<str>, u64)> = by_name.into_iter().collect();
    all.sort_unstable_by(|(a, an), (b, bn)| (bn, a).cmp(&(an, b)));
    let rest = all
        .iter()
        .skip(SERVICES)
        .map(|(_, n)| *n)
        .fold(0, u64::saturating_add);
    let more = all.len().saturating_sub(SERVICES);
    let mut shown: Vec<(Option<String>, u64)> = all
        .into_iter()
        .take(SERVICES)
        .map(|(name, n)| ((!name.is_empty()).then(|| name.into()), n))
        .collect();
    if more > 0 {
        shown.push((Some(format!("({more} more)")), rest));
    }
    shown
}

#[path = "pattern_examples.rs"]
mod examples;
pub use examples::{Examples, MAX_EXAMPLES};

#[cfg(test)]
#[path = "pattern_report_tests.rs"]
mod tests;
