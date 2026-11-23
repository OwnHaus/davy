//! The log search of the Logs page: a page of logs newest first, the pages
//! before it, the logs newer than it for live, and a histogram of the same
//! filter.

use super::{
    Refusal,
    issues::{LogLine, Query, Reads},
    json,
};
use crate::{
    http::internal,
    issues,
    logs::{
        self, LogLevel,
        store::{self as log_store, LogHit, LogQuery},
    },
    time::now_ns,
};
use axum::{
    Router,
    extract::State,
    http::{StatusCode, Uri},
    response::Response,
    routing::get,
};
use serde::Serialize;
use std::{collections::HashMap, sync::Arc};

/// Logs on a page.
pub const PAGE: usize = 100;
/// Characters a search query has at most.
pub const MAX_QUERY: usize = 1000;
/// Logs at one nanosecond a cursor may step over. Past it the next page
/// would have to read them all again for each page.
const MAX_SEEN: usize = 10_000;
/// The window when the browser names no start: the hour up to the end.
const DEFAULT_SPAN: i64 = 3_600_000_000_000;
const FILTER: [&str; 5] = ["q", "from", "to", "project", "level"];

pub fn routes(reads: Arc<Reads>) -> Router {
    Router::new()
        .route("/ui/api/logs", get(list))
        .route("/ui/api/logs/histogram", get(histogram))
        .with_state(reads)
}

/// Where an older page starts: at `ts` and before, past the `seen` logs at
/// exactly `ts` that the newer pages showed. Encoded it is opaque to the
/// browser.
#[derive(Debug, PartialEq, Eq)]
struct Before {
    ts: i64,
    seen: usize,
}

impl Before {
    fn parse(s: &str) -> Option<Self> {
        let (ts, seen) = s.split_once('.')?;
        Some(Self {
            ts: ts.parse().ok().filter(|ts| *ts >= 0)?,
            seen: seen.parse().ok().filter(|n| (1..=MAX_SEEN).contains(n))?,
        })
    }

    fn encode(&self) -> String {
        format!("{}.{}", self.ts, self.seen)
    }
}

/// A time in the query string, in nanoseconds since 1970.
fn ns(q: &Query, key: &str) -> Result<Option<i64>, Refusal> {
    let parse = |v: &str| {
        v.parse::<i64>()
            .ok()
            .filter(|n| *n >= 0)
            .ok_or_else(|| Refusal::bad(format!("{key} is a time in nanoseconds since 1970")))
    };
    q.get(key).map(parse).transpose()
}

/// The filter both routes take, and the slug of the project it names.
fn filter(q: &Query) -> Result<(LogQuery, Option<String>), Refusal> {
    let until = ns(q, "to")?.unwrap_or_else(now_ns);
    let since = ns(q, "from")?.unwrap_or(until.saturating_sub(DEFAULT_SPAN));
    if since > until {
        return Err(Refusal::bad("from is after to"));
    }
    let min_level = q.get("level").map(LogLevel::named).transpose();
    let text = q.owned("q");
    if let Some(text) = &text {
        if text.chars().count() > MAX_QUERY {
            let why = format!("the query has at most {MAX_QUERY} characters");
            return Err(Refusal::bad(why));
        }
        log_store::check_text(text).map_err(Refusal::bad)?;
    }
    let query = LogQuery {
        project: None,
        since,
        until,
        min_level: min_level.map_err(Refusal::bad)?,
        text,
        limit: PAGE,
    };
    Ok((query, q.owned("project")))
}

/// Every project's slug by id, and the id of `slug`.
async fn projects(
    reads: &Arc<Reads>,
    slug: Option<String>,
) -> Result<(HashMap<i64, String>, Option<i64>), Refusal> {
    let slugs = reads.run(issues::slugs).await?;
    let Some(slug) = slug else {
        return Ok((slugs, None));
    };
    let id = slugs.iter().find(|(_, s)| **s == slug).map(|(id, _)| *id);
    let id = id.ok_or_else(|| Refusal::new(StatusCode::NOT_FOUND, format!("no project {slug}")))?;
    Ok((slugs, Some(id)))
}

/// Runs a log store read off the async threads, holding a search place.
async fn searching<T: Send + 'static>(
    reads: &Arc<Reads>,
    f: impl FnOnce(&std::path::Path) -> anyhow::Result<T> + Send + 'static,
) -> Result<T, Refusal> {
    let permit = reads.search_permit()?;
    let dir = logs::dir(reads.data());
    let task = move || {
        let _permit = permit;
        f(&dir)
    };
    Ok(tokio::task::spawn_blocking(task)
        .await
        .map_err(internal)?
        .map_err(internal)?)
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct LogPage {
    /// Newest first.
    pub logs: Vec<LogLine>,
    /// The cursor of the page before this one, when it is full.
    pub before: Option<String>,
}

async fn list(State(reads): State<Arc<Reads>>, uri: Uri) -> Result<Response, Refusal> {
    let q = Query::parse(&uri, &[&FILTER[..], &["before", "after"]].concat())?;
    let (mut query, slug) = filter(&q)?;
    let before = q.get("before").map(|b| {
        Before::parse(b).ok_or_else(|| Refusal::bad("before is the cursor of a page of logs"))
    });
    match (before.transpose()?, ns(&q, "after")?) {
        (Some(_), Some(_)) => Err(Refusal::bad("a page is before or after, not both")),
        (Some(before), None) => {
            query.until = query.until.min(before.ts);
            query.limit = PAGE + before.seen;
            read_page(&reads, query, slug, Some(before)).await
        }
        (None, after) => {
            if let Some(after) = after {
                query.since = query.since.max(after.saturating_add(1));
            }
            read_page(&reads, query, slug, None).await
        }
    }
}

async fn read_page(
    reads: &Arc<Reads>,
    mut query: LogQuery,
    slug: Option<String>,
    before: Option<Before>,
) -> Result<Response, Refusal> {
    let (slugs, project) = projects(reads, slug).await?;
    query.project = project;
    let page = searching(reads, move |dir| {
        if query.since > query.until {
            return Ok(Vec::new());
        }
        log_store::search(dir, &query)
    })
    .await?;
    json(&page_of(page, PAGE, before.as_ref(), &slugs))
}

/// The page of `hits`, oldest first as the store gives them, past the logs
/// the newer page showed, and the cursor of the page before it.
fn page_of(
    mut hits: Vec<LogHit>,
    page: usize,
    before: Option<&Before>,
    slugs: &HashMap<i64, String>,
) -> LogPage {
    hits.reverse();
    let skip = before.map_or(0, |b| {
        let at = hits.iter().take_while(|h| h.timestamp_ns == b.ts).count();
        at.min(b.seen)
    });
    let older = (hits.len() - skip >= page)
        .then(|| hits.last())
        .flatten()
        .map(|oldest| Before {
            ts: oldest.timestamp_ns,
            seen: hits
                .iter()
                .filter(|h| h.timestamp_ns == oldest.timestamp_ns)
                .count()
                .min(MAX_SEEN),
        });
    LogPage {
        logs: hits
            .into_iter()
            .skip(skip)
            .map(|h| LogLine::new(h, slugs))
            .collect(),
        before: older.map(|b| b.encode()),
    }
}

/// How many logs the filter matches in round buckets, at most
/// [`log_store::MAX_BUCKETS`]. Times are decimal strings of nanoseconds.
#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct LogHistogram {
    pub from: String,
    pub step: String,
    pub counts: Vec<u64>,
}

async fn histogram(State(reads): State<Arc<Reads>>, uri: Uri) -> Result<Response, Refusal> {
    let q = Query::parse(&uri, &FILTER)?;
    let (mut query, slug) = filter(&q)?;
    query.project = projects(&reads, slug).await?.1;
    let h = searching(&reads, move |dir| log_store::histogram(dir, &query)).await?;
    json(&LogHistogram {
        from: h.from.to_string(),
        step: h.step.to_string(),
        counts: h.counts,
    })
}
