//! The issue list a person pages through, the events of one issue, and
//! status changes of many issues at once.

use crate::{
    event::Timestamp,
    store::{IssueStatus, Outcome},
    time,
};
use anyhow::Result;
use rusqlite::{
    Connection, OptionalExtension, params, params_from_iter,
    types::{FromSql, FromSqlError, FromSqlResult, ValueRef},
};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, fmt::Write as _};

/// Issues on one page of the list.
pub const PAGE: usize = 50;
/// Events on one page of an issue's events.
pub const EVENT_PAGE: usize = 50;
/// Issues one status change may name.
pub const MAX_BULK: usize = 500;
/// The longest text a list filter matches, in characters.
pub const MAX_TEXT: usize = 200;
/// Cells of the hourly chart of a row, the last one the current hour.
pub const HOURS: usize = 24;
const HOUR_MS: Timestamp = 3_600_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum Sort {
    #[default]
    LastSeen,
    Events,
    FirstSeen,
}

impl Sort {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "last_seen" => Some(Self::LastSeen),
            "events" => Some(Self::Events),
            "first_seen" => Some(Self::FirstSeen),
            _ => None,
        }
    }

    fn column(self) -> &'static str {
        match self {
            Self::LastSeen => "i.last_seen",
            Self::Events => "i.event_count",
            Self::FirstSeen => "i.first_seen",
        }
    }

    fn tag(self) -> char {
        match self {
            Self::LastSeen => 'l',
            Self::Events => 'e',
            Self::FirstSeen => 'f',
        }
    }
}

/// What a person sees of [`IssueStatus`], and what a list filters on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Unresolved,
    Resolved,
    Ignored,
}

impl Status {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "unresolved" => Some(Self::Unresolved),
            "resolved" => Some(Self::Resolved),
            "ignored" => Some(Self::Ignored),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unresolved => "unresolved",
            Self::Resolved => "resolved",
            Self::Ignored => "ignored",
        }
    }
}

impl FromSql for Status {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        Self::parse(value.as_str()?).ok_or(FromSqlError::InvalidType)
    }
}

/// Which issues a list holds. Every field narrows it.
#[derive(Debug, Default)]
pub struct Filter {
    pub project: Option<i64>,
    pub status: Option<Status>,
    pub environment: Option<String>,
    pub release: Option<String>,
    /// Matched in title and culprit, ignoring case. Below three characters
    /// only ASCII case is ignored.
    pub text: Option<String>,
}

/// Where a page of the list starts: after the issue with this sort key and
/// id. A person passes it back as an opaque string.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    sort: Sort,
    key: i64,
    id: i64,
}

impl Cursor {
    /// `None` for a string [`Cursor::encode`] did not make.
    pub fn parse(s: &str) -> Option<Self> {
        let mut chars = s.chars();
        let tag = chars.next()?;
        let sort = [Sort::LastSeen, Sort::Events, Sort::FirstSeen]
            .into_iter()
            .find(|sort| sort.tag() == tag)?;
        let (key, id) = chars.as_str().split_once('.')?;
        Some(Self {
            sort,
            key: key.parse().ok()?,
            id: id.parse().ok()?,
        })
    }

    pub fn encode(&self) -> String {
        format!("{}{}.{}", self.sort.tag(), self.key, self.id)
    }

    pub fn sort(&self) -> Sort {
        self.sort
    }
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct IssueRow {
    pub id: i64,
    pub project: String,
    pub title: String,
    pub culprit: Option<String>,
    pub level: String,
    pub status: Status,
    pub count: i64,
    pub first_seen: String,
    pub last_seen: String,
    /// Events in each of the [`HOURS`] hours from `IssuePage::hours_from`.
    pub hourly: Vec<u32>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct IssuePage {
    pub issues: Vec<IssueRow>,
    /// The cursor of the next page, when there is one.
    pub next: Option<String>,
    /// The start of the first hour of every row's hourly chart.
    pub hours_from: String,
}

/// The first hour of a chart of [`HOURS`] hours that ends with the hour of
/// `now`.
#[expect(clippy::cast_possible_wrap, reason = "HOURS is 24")]
pub fn hours_from(now: Timestamp) -> Timestamp {
    (now.div_euclid(HOUR_MS) - (HOURS as i64 - 1)) * HOUR_MS
}

/// One page of issues, sorted by `sort` with the newest id first among
/// equal keys. `after` is where the page starts.
pub fn list(
    c: &Connection,
    filter: &Filter,
    sort: Sort,
    after: Option<Cursor>,
    now: Timestamp,
) -> Result<IssuePage> {
    let column = sort.column();
    let (sql, args) = page_ids(filter, column, after)?;
    let sql = format!(
        "SELECT i.id,p.slug,i.title,i.culprit,i.level,i.status,i.event_count,i.first_seen,i.last_seen,{column} FROM ({sql}) t JOIN issues i ON i.id=t.id JOIN projects p ON p.id=i.project_id ORDER BY {column} DESC,i.id DESC"
    );
    let from = hours_from(now);
    let mut q = c.prepare(&sql)?;
    let mut keys = Vec::new();
    let mut issues = q
        .query_map(params_from_iter(&args), |r| {
            keys.push(r.get::<_, i64>(9)?);
            Ok(IssueRow {
                id: r.get(0)?,
                project: r.get(1)?,
                title: r.get(2)?,
                culprit: r.get(3)?,
                level: r.get(4)?,
                status: r.get(5)?,
                count: r.get(6)?,
                first_seen: time::rfc3339_ms(r.get(7)?),
                last_seen: time::rfc3339_ms(r.get(8)?),
                hourly: vec![0; HOURS],
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let next = (issues.len() > PAGE).then(|| {
        issues.truncate(PAGE);
        Cursor {
            sort,
            key: keys[PAGE - 1],
            id: issues[PAGE - 1].id,
        }
        .encode()
    });
    fill_hourly(c, &mut issues, from)?;
    Ok(IssuePage {
        issues,
        next,
        hours_from: time::rfc3339_ms(from),
    })
}

/// The query of one page's ids and its parameters. The ids come from an
/// index alone, so the columns of the rows are read for those ids only.
fn page_ids(
    filter: &Filter,
    column: &str,
    after: Option<Cursor>,
) -> Result<(String, Vec<rusqlite::types::Value>)> {
    let mut sql = String::from("SELECT i.id FROM issues i WHERE 1");
    let mut args: Vec<rusqlite::types::Value> = Vec::new();
    // Each value becomes a numbered parameter, so the short text filter can
    // be used twice.
    let mut bind = |value: rusqlite::types::Value| {
        args.push(value);
        format!("?{}", args.len())
    };
    if let Some(project) = filter.project {
        write!(sql, " AND i.project_id={}", bind(project.into()))?;
    }
    if let Some(status) = filter.status {
        write!(
            sql,
            " AND i.status={}",
            bind(status.as_str().to_owned().into())
        )?;
    }
    for (key, value) in [
        ("environment", &filter.environment),
        ("release", &filter.release),
    ] {
        if let Some(value) = value {
            let (k, v) = (bind(key.to_owned().into()), bind(value.clone().into()));
            write!(
                sql,
                " AND i.id IN (SELECT issue_id FROM issue_tags WHERE key={k} AND value={v})"
            )?;
        }
    }
    if let Some(text) = &filter.text {
        // The trigram index finds substrings of three characters or more.
        if text.chars().count() >= 3 {
            let p = bind(format!("\"{}\"", text.replace('"', "\"\"")).into());
            write!(
                sql,
                " AND i.id IN (SELECT rowid FROM issues_text WHERE issues_text MATCH {p})"
            )?;
        } else {
            let p = bind(text.to_ascii_lowercase().into());
            write!(
                sql,
                " AND (instr(lower(i.title),{p})>0 OR instr(lower(coalesce(i.culprit,'')),{p})>0)"
            )?;
        }
    }
    if let Some(after) = after {
        let (key, id) = (bind(after.key.into()), bind(after.id.into()));
        write!(sql, " AND ({column},i.id)<({key},{id})")?;
    }
    let limit = bind(i64::try_from(PAGE + 1)?.into());
    write!(sql, " ORDER BY {column} DESC,i.id DESC LIMIT {limit}")?;
    Ok((sql, args))
}

fn fill_hourly(c: &Connection, issues: &mut [IssueRow], from: Timestamp) -> Result<()> {
    let ids = serde_json::to_string(&issues.iter().map(|i| i.id).collect::<Vec<_>>())?;
    let mut q = c.prepare(
        "SELECT issue_id,hour,count FROM issue_hourly WHERE issue_id IN (SELECT value FROM json_each(?1)) AND hour>=?2",
    )?;
    let rows = q.query_map(params![ids, from], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, u32>(2)?,
        ))
    })?;
    for row in rows {
        let (id, hour, count) = row?;
        let cell = usize::try_from((hour - from) / HOUR_MS).ok();
        let issue = issues.iter_mut().find(|i| i.id == id);
        if let (Some(cell), Some(issue)) = (cell, issue)
            && let Some(slot) = issue.hourly.get_mut(cell)
        {
            *slot = count;
        }
    }
    Ok(())
}

/// The events per hour of one issue, for the [`HOURS`] hours from `from`.
pub fn hourly(c: &Connection, issue: i64, from: Timestamp) -> Result<Vec<u32>> {
    let mut cells = vec![0; HOURS];
    let mut q = c.prepare("SELECT hour,count FROM issue_hourly WHERE issue_id=?1 AND hour>=?2")?;
    let rows = q.query_map(params![issue, from], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, u32>(1)?))
    })?;
    for row in rows {
        let (hour, count) = row?;
        if let Some(slot) = usize::try_from((hour - from) / HOUR_MS)
            .ok()
            .and_then(|cell| cells.get_mut(cell))
        {
            *slot = count;
        }
    }
    Ok(cells)
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct EventItem {
    pub event_id: String,
    pub timestamp: String,
    pub release: Option<String>,
    pub environment: Option<String>,
    /// Whether Davy kept the payload. Only a sample of events keep one.
    pub stored: bool,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct EventPage {
    pub events: Vec<EventItem>,
    pub next: Option<String>,
}

/// One page of the events of `issue`, newest first. `after` is the
/// cursor of the page before.
pub fn events(c: &Connection, issue: i64, after: Option<i64>) -> Result<EventPage> {
    let mut q = c.prepare(
        "SELECT id,event_id,timestamp,release,environment,payload IS NOT NULL FROM events WHERE issue_id=?1 AND id<?2 ORDER BY id DESC LIMIT ?3",
    )?;
    let limit = i64::try_from(EVENT_PAGE + 1)?;
    let mut ids = Vec::new();
    let mut events = q
        .query_map(params![issue, after.unwrap_or(i64::MAX), limit], |r| {
            ids.push(r.get::<_, i64>(0)?);
            Ok(EventItem {
                event_id: r.get(1)?,
                timestamp: time::rfc3339_ms(r.get(2)?),
                release: r.get(3)?,
                environment: r.get(4)?,
                stored: r.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let next = (events.len() > EVENT_PAGE).then(|| {
        events.truncate(EVENT_PAGE);
        ids[EVENT_PAGE - 1].to_string()
    });
    Ok(EventPage { events, next })
}

/// The compressed payload of the event `event_id` of `issue`, or of its
/// newest stored event, with the id of the event it belongs to.
pub fn stored_payload(
    c: &Connection,
    issue: i64,
    event_id: Option<&str>,
) -> Result<Option<(String, Vec<u8>)>> {
    Ok(c.query_row(
        "SELECT event_id,payload FROM events WHERE issue_id=?1 AND payload IS NOT NULL AND (?2 IS NULL OR event_id=?2) ORDER BY id DESC LIMIT 1",
        params![issue, event_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()?)
}

/// Releases a project lists for the release filter, the newest first.
pub const RELEASES: usize = 50;

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct Project {
    pub slug: String,
    /// The [`RELEASES`] releases seen first most recently.
    pub releases: Vec<String>,
}

/// Every project by slug, with its newest releases.
pub fn projects(c: &Connection) -> Result<Vec<Project>> {
    let mut q = c.prepare("SELECT id,slug FROM projects ORDER BY slug")?;
    let all = q
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut releases = c.prepare(
        "SELECT version FROM releases WHERE project_id=?1 ORDER BY first_seen DESC,version DESC LIMIT ?2",
    )?;
    let limit = i64::try_from(RELEASES)?;
    all.into_iter()
        .map(|(id, slug)| {
            let releases = releases
                .query_map(params![id, limit], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<String>>>()?;
            Ok(Project { slug, releases })
        })
        .collect()
}

/// Every project's slug by its id.
pub fn slugs(c: &Connection) -> Result<HashMap<i64, String>> {
    let mut q = c.prepare("SELECT id,slug FROM projects")?;
    let rows = q.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Where and when an event of `issue` happened.
#[derive(Debug, PartialEq, Eq)]
pub struct Moment {
    pub project: i64,
    pub at: Timestamp,
    pub trace_id: Option<String>,
}

/// The [`Moment`] of the event `event_id` of `issue`, or of its newest
/// stored event.
pub fn moment(c: &Connection, issue: i64, event_id: Option<&str>) -> Result<Option<Moment>> {
    Ok(c.query_row(
        "SELECT project_id,timestamp,trace_id FROM events WHERE issue_id=?1 AND (?2 IS NULL AND payload IS NOT NULL OR event_id=?2) ORDER BY id DESC LIMIT 1",
        params![issue, event_id],
        |r| {
            Ok(Moment {
                project: r.get(0)?,
                at: r.get(1)?,
                trace_id: r.get(2)?,
            })
        },
    )
    .optional()?)
}

/// The id of the first of `issues` that does not exist.
pub fn missing(c: &Connection, issues: &IssueIds) -> Result<Option<i64>> {
    let ids = serde_json::to_string(&issues.0)?;
    Ok(c.query_row(
        "SELECT value FROM json_each(?1) WHERE value NOT IN (SELECT id FROM issues) LIMIT 1",
        [ids],
        |r| r.get(0),
    )
    .optional()?)
}

/// One to [`MAX_BULK`] issue ids, each once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssueIds(Vec<i64>);

impl IssueIds {
    pub fn new(mut ids: Vec<i64>) -> std::result::Result<Self, String> {
        ids.sort_unstable();
        ids.dedup();
        match ids.len() {
            0 => Err("name at least one issue".to_owned()),
            n if n > MAX_BULK => Err(format!(
                "a change names at most {MAX_BULK} issues, this one {n}"
            )),
            _ => Ok(Self(ids)),
        }
    }

    pub fn one(id: i64) -> Self {
        Self(vec![id])
    }
}

/// Sets the status of every one of `issues`, or of none when one of them
/// does not exist. Reopening or ignoring forgets the release an issue was
/// resolved in.
pub fn set_status(c: &Connection, issues: &IssueIds, status: &IssueStatus) -> Result<Outcome> {
    if let Some(id) = missing(c, issues)? {
        return Ok(Err(format!("no issue {id}")));
    }
    let (in_release, at) = match status {
        IssueStatus::Resolved { in_release, at } => (in_release.as_deref(), Some(*at)),
        IssueStatus::Unresolved | IssueStatus::Ignored => (None, None),
    };
    c.execute(
        "UPDATE issues SET status=?2,resolved_in_release=?3,resolved_at=?4 WHERE id IN (SELECT value FROM json_each(?1))",
        params![
            serde_json::to_string(&issues.0)?,
            status.as_str(),
            in_release,
            at
        ],
    )?;
    Ok(Ok(()))
}

#[cfg(test)]
#[path = "issues_tests.rs"]
mod tests;
