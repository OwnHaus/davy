//! What each project stored, and what became of what it sent, per UTC day
//! of a window, for the usage page of the web UI (`docs/design.md`, "The
//! usage page"). One query per counting table reads the window's rows; none
//! walks the disk.

use super::Signal;
use crate::{items::client_report::OWN_PREFIX, retention, time};
use anyhow::{Result, anyhow};
use rusqlite::Connection;
use serde::Serialize;
use std::collections::HashMap;

/// Projects the days name, oldest first. The rest are summed as one
/// Other, so a page draws at most this many bands and one more however
/// many projects there are.
pub const NAMED: usize = 8;
/// Projects of Other named with what each stored, the most first.
pub const OTHER_TOP: usize = 3;
/// The client report reasons of Davy's own rate limits and daily caps,
/// counted as `limited`.
pub const LIMITED: [&str; 2] = ["davy_rate", "davy_daily_cap"];
/// The client report reasons of check-ins Davy could not read, too large
/// or not valid, counted as `unreadable`: unlike every other reason of
/// its own, they are written for items ingest did not count as accepted.
pub const UNREADABLE: [&str; 2] = ["davy_too_large", "davy_invalid"];
/// Client report categories that count bytes or time, not items. Their
/// quantities are left out, so a 5 KB log line an SDK dropped counts as
/// one item, not 5,001.
pub const NOT_ITEMS: [&str; 5] = [
    "attachment",
    "log_byte",
    "profile_duration",
    "profile_duration_ui",
    "metric_second",
];

/// The days of a window, and each project's counts in their order.
#[derive(Debug, PartialEq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct UsageDays {
    /// Each UTC day of the window, oldest first, today last.
    pub days: Vec<String>,
    /// The slug of every project, oldest first, so a page can list them
    /// all and give each the same place whichever is asked for.
    pub names: Vec<String>,
    /// The first [`NAMED`] projects, oldest first, or the one asked for.
    pub projects: Vec<ProjectDays>,
    /// The projects past the first [`NAMED`], summed, or `None` when there
    /// are no more or one project was asked for.
    pub other: Option<OtherDays>,
}

/// One project's counts.
#[derive(Debug, Default, PartialEq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct ProjectDays {
    pub project: String,
    #[serde(flatten)]
    #[cfg_attr(test, ts(flatten))]
    pub counts: Counts,
}

/// The projects past the first [`NAMED`], summed.
#[derive(Debug, Default, PartialEq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct OtherDays {
    /// How many projects it sums.
    pub projects: usize,
    /// The [`OTHER_TOP`] of them that stored the most bytes in the window,
    /// the most first, a tie by age.
    pub top: Vec<ProjectStored>,
    #[serde(flatten)]
    #[cfg_attr(test, ts(flatten))]
    pub counts: Counts,
}

/// What one project stored per day.
#[derive(Debug, Default, PartialEq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct ProjectStored {
    pub project: String,
    pub stored: Vec<u64>,
}

/// Counts, each a list in the order of [`UsageDays::days`]. `accepted`
/// holds `filtered`, `discarded`, `limited` and `refused`: ingest counted
/// those items and Davy then did not store them. `unreadable` and
/// `sdk_dropped` were never accepted. Every count is of items: client
/// reports in bytes or time, [`NOT_ITEMS`], are left out.
#[derive(Debug, Default, PartialEq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct Counts {
    /// Raw bytes of the logs stored on the UTC day of their index, a line
    /// indexed for tomorrow on today, and the bytes of the errors,
    /// sessions, feedback and check-ins accepted that day.
    pub stored: Vec<u64>,
    /// Records accepted, of every signal.
    pub accepted: Vec<u64>,
    /// Log lines live drop rules dropped.
    pub filtered: Vec<u64>,
    /// Error events of discarded issues, counted and not stored.
    pub discarded: Vec<u64>,
    /// Items Davy refused past one of its rate limits or daily caps.
    pub limited: Vec<u64>,
    /// Items Davy accepted and then refused for another reason, such as a
    /// check-in for a monitor that does not exist.
    pub refused: Vec<u64>,
    /// Check-ins Davy could not read, too large or not valid.
    pub unreadable: Vec<u64>,
    /// Items the SDKs reported they dropped before sending.
    pub sdk_dropped: Vec<u64>,
}

impl Counts {
    fn zeros(n: usize) -> Self {
        let z = vec![0; n];
        Self {
            stored: z.clone(),
            accepted: z.clone(),
            filtered: z.clone(),
            discarded: z.clone(),
            limited: z.clone(),
            refused: z.clone(),
            unreadable: z.clone(),
            sdk_dropped: z,
        }
    }
}

/// Every project, or the one named `project`, per UTC day from the day of
/// `since_ns` to the day of `now_ns`.
pub fn days(
    c: &Connection,
    project: Option<&str>,
    since_ns: i64,
    now_ns: i64,
) -> Result<UsageDays> {
    let from = time::day_of(since_ns);
    let today = time::day_of(now_ns);
    let mut days = Vec::new();
    let mut day = from;
    while day <= today {
        days.push(day.to_string());
        day = day.tomorrow()?;
    }
    let mut grid = Grid::new(c, project, &days)?;
    // A line may be indexed up to a day ahead, see
    // `logs::store::State::settle`; it counts on today.
    let tomorrow = today.tomorrow()?.to_string();
    grid.days
        .insert(tomorrow.clone(), days.len().saturating_sub(1));
    let (first, last) = (from.to_string(), today.to_string());
    stored_logs(c, &mut grid, &first, &tomorrow)?;
    accepted(c, &mut grid, &first, &last)?;
    lost(c, &mut grid, &first, &last)?;
    client_reports(
        c,
        &mut grid,
        time::midnight(from) / 1_000_000,
        time::midnight(today) / 1_000_000,
    )?;
    Ok(grid.finish(days))
}

/// Where a project's rows add up: its own counts, or Other's.
#[derive(Clone, Copy)]
enum Row {
    Named(usize),
    /// Other, and where the project sits in `Grid::folded`.
    Other(usize),
}

/// The projects asked for, every count 0, and where each day and project
/// sits in them.
struct Grid {
    days: HashMap<String, usize>,
    rows: HashMap<i64, Row>,
    names: Vec<String>,
    projects: Vec<ProjectDays>,
    other: Option<OtherDays>,
    /// What each project summed in Other stored, in the order of their ids.
    folded: Vec<ProjectStored>,
    /// The id of the one project asked for.
    only: Option<i64>,
}

impl Grid {
    fn new(c: &Connection, project: Option<&str>, days: &[String]) -> Result<Self> {
        let mut q = c.prepare("SELECT id, slug FROM projects ORDER BY id")?;
        let all = q
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let only = match project {
            Some(slug) => Some(
                all.iter()
                    .find(|(_, s)| s == slug)
                    .map(|(id, _)| *id)
                    .ok_or_else(|| anyhow!(retention::unknown(slug)))?,
            ),
            None => None,
        };
        let n = days.len();
        let mut grid = Self {
            days: days
                .iter()
                .enumerate()
                .map(|(i, d)| (d.clone(), i))
                .collect(),
            rows: HashMap::new(),
            names: all.iter().map(|(_, s)| s.clone()).collect(),
            projects: Vec::new(),
            other: None,
            folded: Vec::new(),
            only,
        };
        for (id, slug) in all {
            if only.is_some_and(|o| o != id) {
                continue;
            }
            let row = if grid.projects.len() < NAMED {
                grid.projects.push(ProjectDays {
                    project: slug,
                    counts: Counts::zeros(n),
                });
                Row::Named(grid.projects.len() - 1)
            } else {
                let other = grid.other.get_or_insert_with(|| OtherDays {
                    counts: Counts::zeros(n),
                    ..OtherDays::default()
                });
                other.projects += 1;
                grid.folded.push(ProjectStored {
                    project: slug,
                    stored: vec![0; n],
                });
                Row::Other(grid.folded.len() - 1)
            };
            grid.rows.insert(id, row);
        }
        Ok(grid)
    }

    /// Adds `n` to the count `field` picks of project `id` on `day`. A row
    /// of a day or project outside the grid adds nothing.
    fn add(&mut self, id: i64, day: &str, n: i64, field: fn(&mut Counts) -> &mut Vec<u64>) {
        let (Some(&row), Some(&at)) = (self.rows.get(&id), self.days.get(day)) else {
            return;
        };
        let counts = match row {
            Row::Named(i) => self.projects.get_mut(i).map(|p| &mut p.counts),
            Row::Other(_) => self.other.as_mut().map(|o| &mut o.counts),
        };
        if let Some(cell) = counts.and_then(|c| field(c).get_mut(at)) {
            *cell = cell.saturating_add(u64::try_from(n).unwrap_or(0));
        }
    }

    /// Adds `n` stored bytes, to Other's sum and to the project's own when
    /// it sits in Other.
    fn store(&mut self, id: i64, day: &str, n: i64) {
        self.add(id, day, n, |c| &mut c.stored);
        if let (Some(&Row::Other(i)), Some(&at)) = (self.rows.get(&id), self.days.get(day))
            && let Some(cell) = self.folded.get_mut(i).and_then(|p| p.stored.get_mut(at))
        {
            *cell = cell.saturating_add(u64::try_from(n).unwrap_or(0));
        }
    }

    /// The days, with the [`OTHER_TOP`] projects of Other named.
    fn finish(mut self, days: Vec<String>) -> UsageDays {
        if let Some(other) = self.other.as_mut() {
            let total = |p: &ProjectStored| p.stored.iter().fold(0u64, |a, b| a.saturating_add(*b));
            // A stable sort keeps a tie in the order of the ids.
            self.folded.sort_by_key(|p| std::cmp::Reverse(total(p)));
            self.folded.truncate(OTHER_TOP);
            other.top = self.folded;
        }
        UsageDays {
            days,
            names: self.names,
            projects: self.projects,
            other: self.other,
        }
    }
}

fn stored_logs(c: &Connection, grid: &mut Grid, first: &str, last: &str) -> Result<()> {
    let mut q = c.prepare(
        "SELECT day, project_id, bytes FROM log_days
         WHERE day>=?1 AND day<=?2 AND (?3 IS NULL OR project_id=?3)",
    )?;
    let mut rows = q.query((first, last, grid.only))?;
    while let Some(r) = rows.next()? {
        let day: String = r.get(0)?;
        grid.store(r.get(1)?, &day, r.get(2)?);
    }
    Ok(())
}

/// Records of every signal, and the bytes of all but logs, whose stored
/// bytes `log_days` counts.
fn accepted(c: &Connection, grid: &mut Grid, first: &str, last: &str) -> Result<()> {
    let mut q = c.prepare(
        "SELECT day, project_id, signal, records, bytes FROM usage_daily
         WHERE day>=?1 AND day<=?2 AND (?3 IS NULL OR project_id=?3)",
    )?;
    let mut rows = q.query((first, last, grid.only))?;
    while let Some(r) = rows.next()? {
        let (day, id, signal): (String, i64, String) = (r.get(0)?, r.get(1)?, r.get(2)?);
        grid.add(id, &day, r.get(3)?, |c| &mut c.accepted);
        if signal != Signal::Logs.name() {
            grid.store(id, &day, r.get(4)?);
        }
    }
    Ok(())
}

/// Lines live drop rules dropped, and error events of discarded issues.
fn lost(c: &Connection, grid: &mut Grid, first: &str, last: &str) -> Result<()> {
    let mut q = c.prepare(
        "SELECT day, project_id, records FROM drop_daily
         WHERE live=1 AND day>=?1 AND day<=?2 AND (?3 IS NULL OR project_id=?3)",
    )?;
    let mut rows = q.query((first, last, grid.only))?;
    while let Some(r) = rows.next()? {
        let day: String = r.get(0)?;
        grid.add(r.get(1)?, &day, r.get(2)?, |c| &mut c.filtered);
    }
    let mut q = c.prepare(
        "SELECT day, project_id, records FROM discarded_daily
         WHERE day>=?1 AND day<=?2 AND (?3 IS NULL OR project_id=?3)",
    )?;
    let mut rows = q.query((first, last, grid.only))?;
    while let Some(r) = rows.next()? {
        let day: String = r.get(0)?;
        grid.add(r.get(1)?, &day, r.get(2)?, |c| &mut c.discarded);
    }
    Ok(())
}

/// What Davy refused and the SDKs dropped, in items. `client_reports`
/// keys its days by unix milliseconds at midnight UTC.
fn client_reports(c: &Connection, grid: &mut Grid, first_ms: i64, last_ms: i64) -> Result<()> {
    let mut q = c.prepare(
        "SELECT day, project_id, reason, category, quantity FROM client_reports
         WHERE day>=?1 AND day<=?2 AND (?3 IS NULL OR project_id=?3)",
    )?;
    let mut rows = q.query((first_ms, last_ms, grid.only))?;
    while let Some(r) = rows.next()? {
        let category: String = r.get(3)?;
        if NOT_ITEMS.contains(&category.as_str()) {
            continue;
        }
        let day = time::day_of(r.get::<_, i64>(0)?.saturating_mul(1_000_000)).to_string();
        let reason: String = r.get(2)?;
        grid.add(r.get(1)?, &day, r.get(4)?, outcome(&reason));
    }
    Ok(())
}

/// The count a client report reason adds to.
fn outcome(reason: &str) -> fn(&mut Counts) -> &mut Vec<u64> {
    if LIMITED.contains(&reason) {
        |c| &mut c.limited
    } else if UNREADABLE.contains(&reason) {
        |c| &mut c.unreadable
    } else if reason.starts_with(OWN_PREFIX) {
        |c| &mut c.refused
    } else {
        |c| &mut c.sdk_dropped
    }
}

#[cfg(test)]
#[path = "days_tests.rs"]
mod tests;
