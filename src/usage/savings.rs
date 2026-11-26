//! Where logs and errors could take less (`docs/design.md`, "Savings"):
//! the patterns with the most bytes and when a search last asked for each,
//! the log retention each project's searches need, and the issues that
//! send most of a project's error events.

/// The discard mark of an issue, the events discarded per day, the log
/// searches people and agents ran, and per day the searches and the
/// oldest read, of a project or of every project (`project_id` 0). One
/// migration, so a branch that takes the same number renumbers one entry.
pub const MIGRATION: &str = "ALTER TABLE issues ADD COLUMN discarded_at INTEGER;
CREATE TABLE discarded_daily(day TEXT NOT NULL, project_id INTEGER NOT NULL REFERENCES projects(id), records INTEGER NOT NULL, PRIMARY KEY(day, project_id)) WITHOUT ROWID;
CREATE TABLE searches(id INTEGER PRIMARY KEY, at INTEGER NOT NULL, by TEXT NOT NULL, project_id INTEGER, since INTEGER NOT NULL, until INTEGER NOT NULL, level INTEGER, pattern INTEGER, query TEXT);
CREATE TABLE search_days(day TEXT NOT NULL, project_id INTEGER NOT NULL, searches INTEGER NOT NULL, oldest_read_days INTEGER NOT NULL, PRIMARY KEY(day, project_id)) WITHOUT ROWID;";
/// Undoes [`MIGRATION`], for tests that run the migrations again,
/// examples included.
pub const UNDO: &str = "ALTER TABLE issues DROP COLUMN discarded_at; DROP TABLE discarded_daily; DROP TABLE searches; DROP TABLE search_days;";

mod drop;
mod fit;
mod noisy;
mod queried;
pub mod searches;
mod text;

pub use drop::{KEEP_READ, KEEP_UNREAD, Keep, Shadow};
pub use fit::{Fit, LOOK_BACK_DAYS, MIN_HISTORY_DAYS, STEPS};
pub use noisy::{MIN_EVENTS, MIN_SHARE_PERCENT, Noisy};
pub use queried::HISTORY;

use super::{Report, footprint::PRIOR, report::human};
use crate::{
    install::sh,
    logs::{
        self, LogLevel,
        patterns::{self, OTHER},
        store::{PatternQuery, PatternRow, top_patterns},
    },
    retention, time,
};
use anyhow::{Result, anyhow};
use jiff::civil::Date;
use rmcp::schemars::JsonSchema;
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;

/// The entries of each kind the view shows by default.
pub const DEFAULT_TOP: usize = 3;
/// The entries of each kind the view shows at most.
pub const MAX_TOP: usize = 20;
/// What the view cannot see, in each answer.
pub const NOTE: &str = "last_queried is approximate: the newest of the last 1000 searches with text or a pattern filter whose filters find the pattern's example, not the lines the search returned. Only searches on the web UI's logs page and with search_logs and count_logs are recorded: reads by davy logs and issue_context are not seen, nor searches dropped when the writer is busy, so a retention entry may suggest cutting days someone reads. Discarding an issue stores none of its future events, real ones included";

/// What the `usage` tool and `davy usage` answer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, JsonSchema, clap::ValueEnum)]
#[schemars(crate = "rmcp::schemars")]
#[serde(rename_all = "lowercase")]
pub enum View {
    /// Counts, disk and forecast.
    #[default]
    Summary,
    /// Where logs and errors could take less.
    Savings,
}

pub struct Request {
    pub project: Option<String>,
    /// Entries of each kind, 1 to [`MAX_TOP`].
    pub top: usize,
    pub defaults: retention::Defaults,
}

/// A log pattern with many bytes.
#[derive(Debug, PartialEq)]
pub struct Pattern {
    pub id: String,
    /// The project asked for, `None` for patterns of every project.
    pub project: Option<String>,
    /// The service and the level with the most of its records.
    pub service: Option<String>,
    pub level: Option<&'static str>,
    pub example: String,
    /// Raw bytes in the window, and per day of it.
    pub bytes: u64,
    pub per_day: u64,
    /// Of the raw bytes of the window, to two decimals.
    pub share: f64,
    /// RFC 3339, see [`queried::last_queried`].
    pub last_queried: Option<String>,
    /// The share a drop rule would keep, in the view of one project.
    pub keep: Option<Keep>,
    /// The live rule that decides its lines, which keep would otherwise
    /// suggest a rule for.
    pub decided_by: Option<i64>,
}

/// Where logs and errors could take less.
#[derive(Debug, PartialEq)]
pub struct Savings {
    pub since: Date,
    pub days: f64,
    pub patterns: Vec<Pattern>,
    /// The pattern counters of a day could not be read in full, or the
    /// window held more than a report reads, so the bytes may be fewer.
    pub partial: bool,
    pub shadow: Vec<Shadow>,
    pub retention: Vec<Fit>,
    pub issues: Vec<Noisy>,
    /// The data directory, quoted for a shell, for the commands shown.
    pub data: String,
}

/// The savings in the window of `report`, which [`super::prepare`] made
/// with the same project and retention defaults as `req`.
pub fn savings(
    c: &Connection,
    data: &Path,
    report: &Report,
    req: &Request,
    now_ns: i64,
) -> Result<Savings> {
    let top = req.top.clamp(1, MAX_TOP);
    let kept = retention::all_by_id(c, req.defaults, now_ns / 1_000_000)?;
    let filter = match &req.project {
        Some(slug) => Some(
            kept.iter()
                .find(|(_, k)| k.project == *slug)
                .map(|(id, _)| *id)
                .ok_or_else(|| anyhow!(retention::unknown(slug)))?,
        ),
        None => None,
    };
    let start = time::midnight(report.since);
    let (found, partial) =
        patterns_in(c, data, report, (req.project.as_ref(), filter), top, now_ns)?;
    // Davy's own project gets no retention, issue or drop suggestion: its
    // lines are few, so cutting them saves little, and a crash of Davy is
    // to be fixed in Davy, not discarded. Its patterns and shadow rules
    // show like any other's, its shadow rules without a command.
    let own = crate::self_report::own::find(c)?.map(|o| o.id);
    let rules = crate::drops::load(c, filter, now_ns / 1_000_000)?;
    let patterns = drop::with_keep(found, filter.filter(|f| own != Some(*f)), &rules);
    let slug = |id: i64| {
        kept.iter()
            .find(|(k, _)| *k == id)
            .map(|(_, k)| k.project.clone())
            .unwrap_or_default()
    };
    let shadow = drop::shadows(
        c,
        rules,
        (&slug, own),
        report.since,
        now_ns / 1_000_000,
        top,
    )?;
    let ratio = report
        .forecast
        .as_ref()
        .ok()
        .and_then(|f| f.logs.as_ref())
        .and_then(|l| l.ratio)
        .map_or(PRIOR.ratio.mid, |r| r.mid);
    let (mut fits, mut issues) = (Vec::new(), Vec::new());
    for (id, k) in kept
        .iter()
        .filter(|(id, _)| filter.is_none_or(|f| f == *id) && own != Some(*id))
    {
        let project = (*id, k.project.as_str());
        fits.extend(fit::fit(c, project, k.logs.days.get(), ratio, now_ns)?);
        issues.extend(noisy::noisy(c, project, start / 1_000_000, top)?);
    }
    fits.sort_by(|a, b| {
        b.saves
            .cmp(&a.saves)
            .then_with(|| a.project.cmp(&b.project))
    });
    fits.truncate(top);
    issues.sort_by(|a, b| b.events.cmp(&a.events).then(a.id.cmp(&b.id)));
    issues.truncate(top);
    Ok(Savings {
        since: report.since,
        days: report.days,
        patterns,
        partial,
        shadow,
        retention: fits,
        issues,
        data: sh(&std::path::absolute(data)
            .unwrap_or_else(|_| data.to_owned())
            .display()
            .to_string()),
    })
}

/// Patterns, each with the highest level of its lines, and whether a
/// day's counters were read in part.
type Found = (Vec<(Pattern, Option<u8>)>, bool);

/// The `top` patterns with the most bytes in the window, each with the
/// highest level of its lines, and whether a day's counters were read in
/// part.
fn patterns_in(
    c: &Connection,
    data: &Path,
    report: &Report,
    (project, filter): (Option<&String>, Option<i64>),
    top: usize,
    now_ns: i64,
) -> Result<Found> {
    let window = PatternQuery {
        project: filter,
        since: time::midnight(report.since),
        until: now_ns,
        min_level: None,
        // The row of patterns without a count of their own is no pattern.
        limit: top + 1,
    };
    let found = top_patterns(&logs::dir(data), &window)?;
    let partial = found.truncated || !found.unread.is_empty() || !found.restarted.is_empty();
    let other = patterns::show(OTHER);
    let rows: Vec<&PatternRow> = found
        .top
        .iter()
        .filter(|r| r.id != other)
        .take(top)
        .collect();
    let targets: Vec<queried::Target<'_>> = rows
        .iter()
        .map(|r| queried::Target {
            id: patterns::parse(&r.id).unwrap_or(OTHER),
            highest: highest(r),
            example: &r.example,
        })
        .collect();
    let when = queried::last_queried(c, filter, &targets)?;
    let found_patterns = rows
        .into_iter()
        .zip(when)
        .map(|(r, at)| {
            let p = pattern(r, project.cloned(), found.raw_bytes, report.days, at);
            (p, highest(r))
        })
        .collect();
    Ok((found_patterns, partial))
}

/// The highest level of `row`'s records.
fn highest(row: &PatternRow) -> Option<u8> {
    row.levels
        .keys()
        .filter_map(|name| LogLevel::parse(name))
        .map(|l| l as u8)
        .max()
}

fn pattern(
    r: &PatternRow,
    project: Option<String>,
    total: u64,
    days: f64,
    at: Option<i64>,
) -> Pattern {
    Pattern {
        id: r.id.clone(),
        project,
        service: r.services.first().and_then(|(s, _)| s.clone()),
        level: r
            .levels
            .iter()
            .max_by_key(|(_, n)| **n)
            .map(|(name, _)| *name),
        example: r.example.clone(),
        bytes: r.raw_bytes,
        per_day: super::report::bytes_per_day(r.raw_bytes, days),
        share: noisy::share(r.raw_bytes, total),
        last_queried: at.map(time::rfc3339_ms),
        keep: None,
        decided_by: None,
    }
}

impl Savings {
    /// The command that sets the retention `f` suggests, options first
    /// and the slug after `--`, so a slug such as `-x` stays a slug.
    fn retention_command(&self, f: &Fit) -> String {
        format!(
            "davy project retention --logs {} --data {} -- {}",
            f.suggest,
            self.data,
            sh(&f.project)
        )
    }

    /// What a person may do about `f`: the agent only reads.
    fn retention_suggest(&self, f: &Fit) -> String {
        format!(
            "keep {}; a person can run: {}",
            days(f.suggest),
            self.retention_command(f)
        )
    }

    /// The command that discards issue `id`.
    fn discard_command(&self, id: i64) -> String {
        format!("davy issue discard {id} --data {}", self.data)
    }

    /// What a person may do about issue `id`.
    fn discard_suggest(&self, id: i64) -> String {
        format!(
            "fix it, or a person can discard it, which also stops storing its real events: {}",
            self.discard_command(id)
        )
    }

    /// Sizes as [`human`] strings, or with `human` false as bytes.
    pub fn json(&self, human: bool) -> Value {
        let size = |b: u64| {
            if human {
                json!(self::human(b))
            } else {
                json!(b)
            }
        };
        let patterns = self.patterns.iter().map(|p| {
            let mut entry = json!({
                "kind": "pattern",
                "id": p.id,
                "project": p.project,
                "service": p.service,
                "level": p.level,
                "example": p.example,
                "bytes": size(p.bytes),
                "per_day": size(p.per_day),
                "share": p.share,
                "last_queried": p.last_queried,
                "try": format!("search_logs pattern={}", p.id),
            });
            if let (Value::Object(entry), Value::Object(more)) =
                (&mut entry, self.pattern_drop(p, &size))
            {
                entry.extend(more);
            }
            entry
        });
        let shadows = self.shadow.iter().map(|s| self.shadow_json(s, &size));
        let fits = self.retention.iter().map(|f| {
            json!({
                "kind": "retention",
                "project": f.project,
                "logs_days": f.logs_days,
                "oldest_read_days": f.oldest_read_days,
                "searches": f.searches,
                "history_days": f.history_days,
                "suggest": self.retention_suggest(f),
                "command": self.retention_command(f),
                "saves": size(f.saves),
                "try": retention_try(f),
            })
        });
        let issues = self.issues.iter().map(|i| {
            json!({
                "kind": "issue",
                "id": i.id,
                "project": i.project,
                "title": i.title,
                "events": i.events,
                "share": i.share,
                "suggest": self.discard_suggest(i.id),
                "command": self.discard_command(i.id),
                "try": format!("issue_context {}", i.id),
            })
        });
        let mut out = json!({
            "window": {"since": self.since.to_string(), "days": (self.days * 10.0).round() / 10.0},
            "savings": patterns.chain(shadows).chain(fits).chain(issues).collect::<Vec<_>>(),
            "note": NOTE,
        });
        if self.partial {
            out["patterns_partial"] = json!(true);
        }
        out
    }
}

/// `n` days, one day in the singular.
fn days(n: u32) -> String {
    if n == 1 {
        "1 day".to_owned()
    } else {
        format!("{n} days")
    }
}

/// A read-only step for `f`: counts of the logs the cut would delete.
fn retention_try(f: &Fit) -> String {
    format!(
        "count_logs project={} since={}d until={}d",
        Value::from(f.project.as_str()),
        f.logs_days,
        f.suggest
    )
}

#[cfg(test)]
#[path = "savings_tests.rs"]
mod tests;
