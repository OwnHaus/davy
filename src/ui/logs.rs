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
    usage::savings::searches,
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
/// Logs at one nanosecond a cursor may step over. The store passes over
/// them without reading them, but still ranks them for each page. Past this
/// many the next page starts a nanosecond earlier, so paging never stops.
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
            seen: seen.parse().ok().filter(|n| *n < MAX_SEEN)?,
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
        pattern: None,
        limit: PAGE,
        skip: 0,
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
pub(super) async fn searching<T: Send + 'static>(
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
    /// What the search left out of cold days, none when it read them all.
    pub cut: Option<ColdCut>,
}

/// What a read of cold days left out, for the page to say.
#[derive(Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct ColdCut {
    /// When the read ran out of time, in nanoseconds: logs from the start
    /// of the range to here may be missing.
    pub unscanned_to: Option<String>,
    /// Rows, blocks and files left out because they failed a check.
    pub corrupt_rows: u64,
    pub corrupt_blocks: u64,
    pub corrupt_files: u64,
}

impl ColdCut {
    /// The cut of `scan`, none when it read every cold file whole.
    pub fn of(scan: Option<&log_store::cold::ColdScan>) -> Option<Self> {
        let s = scan?;
        let cut = s.unscanned_to_ns.is_some() || s.corrupt_files + s.corrupt_blocks > 0;
        cut.then(|| Self {
            unscanned_to: s.unscanned_to_ns.map(|ns| ns.to_string()),
            corrupt_rows: s.corrupt_rows,
            corrupt_blocks: s.corrupt_blocks,
            corrupt_files: s.corrupt_files,
        })
    }
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
            // A cursor past the end of the window has nothing to skip there.
            if before.ts <= query.until {
                query.until = before.ts;
                query.skip = before.seen;
            }
            read_page(&reads, query, slug, Some(before), false).await
        }
        (None, after) => {
            if let Some(after) = after {
                query.since = query.since.max(after.saturating_add(1));
            }
            // The first page is the search a person ran. The pages before
            // it and the live tail's polls after it are not searches.
            read_page(&reads, query, slug, None, after.is_none()).await
        }
    }
}

async fn read_page(
    reads: &Arc<Reads>,
    mut query: LogQuery,
    slug: Option<String>,
    before: Option<Before>,
    asked: bool,
) -> Result<Response, Refusal> {
    let (slugs, project) = projects(reads, slug).await?;
    query.project = project;
    // Recorded once the search holds its permit, so a search turned away
    // as busy is not counted.
    let writes = asked.then(|| reads.writes().clone());
    let found = searching(reads, move |dir| {
        if let Some(writes) = &writes {
            searches::record(writes, searches::By::Ui, &query);
        }
        if query.since > query.until {
            return Ok(log_store::Found::default());
        }
        log_store::search_for(dir, &query, log_store::cold::BUDGET)
    })
    .await?;
    let mut page = page_of(found.hits, PAGE, before.as_ref(), &slugs);
    page.cut = ColdCut::of(found.cold.as_ref());
    json(&page)
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
    let older = (hits.len() >= page)
        .then(|| hits.last())
        .flatten()
        .and_then(|oldest| {
            let ts = oldest.timestamp_ns;
            let shown = hits.iter().filter(|h| h.timestamp_ns == ts).count();
            let earlier = before.filter(|b| b.ts == ts).map_or(0, |b| b.seen);
            let seen = shown + earlier;
            if seen < MAX_SEEN {
                Some(Before { ts, seen })
            } else {
                (ts > 0).then(|| Before {
                    ts: ts - 1,
                    seen: 0,
                })
            }
        });
    LogPage {
        logs: hits.into_iter().map(|h| LogLine::new(h, slugs)).collect(),
        before: older.map(|b| b.encode()),
        cut: None,
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
    /// What the count left out of cold days, none when it read them all.
    pub cut: Option<ColdCut>,
}

async fn histogram(State(reads): State<Arc<Reads>>, uri: Uri) -> Result<Response, Refusal> {
    let q = Query::parse(&uri, &FILTER)?;
    let (mut query, slug) = filter(&q)?;
    query.project = projects(&reads, slug).await?.1;
    let h = searching(&reads, move |dir| log_store::histogram(dir, &query)).await?;
    json(&LogHistogram {
        from: h.from.to_string(),
        step: h.step.to_string(),
        cut: ColdCut::of(h.cold.as_ref()),
        counts: h.counts,
    })
}
