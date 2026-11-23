//! The issue list, one issue with its events and the logs around them, and
//! status changes of many issues at once.

use super::{Refusal, json, submit};
use crate::{
    context,
    event::EventId,
    event_view::{self, EventView},
    heap,
    http::{Failure, body, internal},
    issues::{self, Cursor, Filter, IssueIds, MAX_TEXT, Sort, Status},
    logs::{
        LogLevel, LogSource,
        store::{LogHit, search},
    },
    store::{self, IssueDetail, IssueStatus, Job, ReleaseCount},
    time,
};
use axum::{
    Router,
    body::Body,
    extract::{Path, State},
    http::{StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Log searches that may run at once.
pub const SEARCHES: usize = 8;
/// What a status change body may weigh: 500 ids and a release.
pub const MAX_STATUS_BODY: usize = 16 * 1024;
const MAX_RELEASE: usize = 200;

/// What the issue routes read from: their own connection, the log
/// directory, and the memory budget ingest uses, because reading a stored
/// event costs memory the way receiving one does.
pub struct Reads {
    db: Mutex<Connection>,
    data: PathBuf,
    budget: body::Budget,
    pub(super) searches: Arc<Semaphore>,
    writes: store::Sender,
}

impl Reads {
    pub fn new(db: Connection, data: PathBuf, budget: body::Budget, writes: store::Sender) -> Self {
        Self {
            db: Mutex::new(db),
            data,
            budget,
            searches: Arc::new(Semaphore::new(SEARCHES)),
            writes,
        }
    }

    /// Runs `f` on the connection off the async threads.
    pub(super) fn data(&self) -> &std::path::Path {
        &self.data
    }

    /// The writer's UI queue.
    pub(super) fn writes(&self) -> &store::Sender {
        &self.writes
    }

    /// A place among the [`SEARCHES`] log searches that may run at once.
    pub(super) fn search_permit(&self) -> Result<OwnedSemaphorePermit, Refusal> {
        Arc::clone(&self.searches)
            .try_acquire_owned()
            .map_err(|_all_busy| {
                Refusal::wait(Duration::from_secs(1), "too many log searches at once")
            })
    }

    pub(super) async fn run<T: Send + 'static>(
        self: &Arc<Self>,
        f: impl FnOnce(&Connection) -> anyhow::Result<T> + Send + 'static,
    ) -> Result<T, Refusal> {
        let reads = Arc::clone(self);
        let task = move || {
            let c = reads.db.lock().map_err(internal)?;
            f(&c).map_err(internal)
        };
        Ok(tokio::task::spawn_blocking(task)
            .await
            .map_err(internal)??)
    }
}

pub fn routes(reads: Arc<Reads>) -> Router {
    Router::new()
        .route("/ui/api/projects", get(projects))
        .route("/ui/api/issues", get(list))
        .route("/ui/api/issues/status", post(set_status))
        .route("/ui/api/issues/{id}", get(detail))
        .route("/ui/api/issues/{id}/events", get(events))
        .route("/ui/api/issues/{id}/event", get(event))
        .route("/ui/api/issues/{id}/logs", get(logs))
        .with_state(Arc::clone(&reads))
        .merge(super::logs::routes(reads))
}

fn not_found(message: impl Into<String>) -> Refusal {
    Refusal::new(StatusCode::NOT_FOUND, message)
}

/// A memory refusal of the shared budget, which clears as other requests
/// finish, so the browser is told when to ask again.
fn priced(failure: Failure) -> Refusal {
    if failure.0 == StatusCode::TOO_MANY_REQUESTS {
        return Refusal::wait(Duration::from_secs(1), failure.1);
    }
    failure.into()
}

/// The query string of a request, each key known and given once. An empty
/// value is the same as none.
pub(super) struct Query(Vec<(String, String)>);

impl Query {
    pub(super) fn parse(uri: &Uri, known: &[&str]) -> Result<Self, Refusal> {
        let mut pairs: Vec<(String, String)> = Vec::new();
        let query = uri.query().unwrap_or_default();
        for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
            if !known.contains(&key.as_ref()) {
                let known = known.join(", ");
                return Err(Refusal::bad(format!("this route takes only {known}")));
            }
            if pairs.iter().any(|(k, _)| *k == key) {
                return Err(Refusal::bad(format!("{key} is given twice")));
            }
            pairs.push((key.into_owned(), value.into_owned()));
        }
        Ok(Self(pairs))
    }

    pub(super) fn get(&self, key: &str) -> Option<&str> {
        let value = self.0.iter().find(|(k, _)| k == key);
        value.map(|(_, v)| v.as_str()).filter(|v| !v.is_empty())
    }

    pub(super) fn owned(&self, key: &str) -> Option<String> {
        self.get(key).map(str::to_owned)
    }

    fn event(&self) -> Result<Option<String>, Refusal> {
        let parse = |id: &str| {
            EventId::parse(id)
                .map(|id| id.0)
                .map_err(|_bad| Refusal::bad("event is an event id, 32 hex digits"))
        };
        self.get("event").map(parse).transpose()
    }
}

fn issue_id(id: &str) -> Result<i64, Refusal> {
    id.parse()
        .ok()
        .filter(|id| *id > 0)
        .ok_or_else(|| not_found(format!("no issue {id}")))
}

/// What `f` reads when there is an issue `id`, else why not.
fn of_issue<T>(
    c: &Connection,
    id: i64,
    f: impl FnOnce() -> anyhow::Result<T>,
) -> anyhow::Result<Result<T, String>> {
    if let Some(id) = issues::missing(c, &IssueIds::one(id))? {
        return Ok(Err(format!("no issue {id}")));
    }
    f().map(Ok)
}

async fn projects(State(reads): State<Arc<Reads>>, uri: Uri) -> Result<Response, Refusal> {
    Query::parse(&uri, &[])?;
    json(&reads.run(issues::projects).await?)
}

async fn list(State(reads): State<Arc<Reads>>, uri: Uri) -> Result<Response, Refusal> {
    let q = Query::parse(
        &uri,
        &[
            "sort",
            "project",
            "status",
            "environment",
            "release",
            "q",
            "cursor",
        ],
    )?;
    let sort = match q.get("sort") {
        None => Sort::default(),
        Some(s) => {
            Sort::parse(s).ok_or_else(|| Refusal::bad("sort is last_seen, events or first_seen"))?
        }
    };
    let status = q.get("status").map(|s| {
        Status::parse(s).ok_or_else(|| Refusal::bad("status is unresolved, resolved or ignored"))
    });
    let text = q.owned("q");
    if text.as_ref().is_some_and(|t| t.chars().count() > MAX_TEXT) {
        let why = format!("the search text has at most {MAX_TEXT} characters");
        return Err(Refusal::bad(why));
    }
    let cursor = q.get("cursor").map(|c| {
        Cursor::parse(c)
            .filter(|c| c.sort() == sort)
            .ok_or_else(|| Refusal::bad("the cursor is not from a list in this order"))
    });
    let mut filter = Filter {
        project: None,
        status: status.transpose()?,
        environment: q.owned("environment"),
        release: q.owned("release"),
        text,
    };
    let (project, cursor) = (q.owned("project"), cursor.transpose()?);
    let page = reads
        .run(move |c| {
            if let Some(slug) = project {
                match store::project_id(c, &slug)? {
                    Some(id) => filter.project = Some(id),
                    None => return Ok(Err(format!("no project {slug}"))),
                }
            }
            issues::list(c, &filter, sort, cursor, time::now_ms()).map(Ok)
        })
        .await?
        .map_err(not_found)?;
    json(&page)
}

/// One issue with its chart and its releases.
#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct IssueView {
    pub issue: IssueDetail,
    pub hourly: Vec<u32>,
    pub hours_from: String,
    pub releases: Vec<ReleaseCount>,
}

async fn detail(
    State(reads): State<Arc<Reads>>,
    Path(id): Path<String>,
    uri: Uri,
) -> Result<Response, Refusal> {
    Query::parse(&uri, &[])?;
    let id = issue_id(&id)?;
    let view = reads
        .run(move |c| {
            let Some(issue) = store::issue(c, id)? else {
                return Ok(None);
            };
            let from = issues::hours_from(time::now_ms());
            Ok(Some(IssueView {
                issue,
                hourly: issues::hourly(c, id, from)?,
                hours_from: time::rfc3339_ms(from),
                releases: store::issue_releases(c, id)?,
            }))
        })
        .await?
        .ok_or_else(|| not_found(format!("no issue {id}")))?;
    json(&view)
}

async fn events(
    State(reads): State<Arc<Reads>>,
    Path(id): Path<String>,
    uri: Uri,
) -> Result<Response, Refusal> {
    let q = Query::parse(&uri, &["cursor"])?;
    let id = issue_id(&id)?;
    let after = q.get("cursor").map(|c| {
        c.parse::<i64>()
            .ok()
            .filter(|n| *n > 0)
            .ok_or_else(|| Refusal::bad("the cursor is not from this list"))
    });
    let after = after.transpose()?;
    let page = reads
        .run(move |c| of_issue(c, id, || issues::events(c, id, after)))
        .await?
        .map_err(not_found)?;
    json(&page)
}

async fn event(
    State(reads): State<Arc<Reads>>,
    Path(id): Path<String>,
    uri: Uri,
) -> Result<Response, Refusal> {
    let q = Query::parse(&uri, &["event"])?;
    let id = issue_id(&id)?;
    let event = q.event()?;
    let stored = reads
        .run(move |c| of_issue(c, id, || issues::stored_payload(c, id, event.as_deref())))
        .await?
        .map_err(not_found)?
        .ok_or_else(|| not_found("Davy kept no payload of that event"))?;
    let mut held = reads.budget.hold();
    let view = tokio::task::spawn_blocking(move || -> Result<EventView, Refusal> {
        let (_, blob) = stored;
        held.take(blob.capacity()).map_err(priced)?;
        let decoded = body::decode(blob, "zstd", &mut held).map_err(priced)?;
        held.take(2 * heap::json(&decoded, 0).map_err(internal)?)
            .map_err(priced)?;
        let raw: Value = serde_json::from_slice(&decoded).map_err(internal)?;
        Ok(event_view::view(&raw))
    })
    .await
    .map_err(internal)??;
    json(&view)
}

/// One log line, its time in nanoseconds as a decimal string because
/// JavaScript numbers lose the last digits.
#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct LogLine {
    pub timestamp: String,
    pub timestamp_ns: String,
    pub project: String,
    pub source: LogSource,
    pub level: LogLevel,
    pub service: Option<String>,
    pub message: String,
    pub trace_id: Option<String>,
    pub span_id: Option<String>,
    pub attributes: Map<String, Value>,
}

impl LogLine {
    pub(super) fn new(h: LogHit, slugs: &HashMap<i64, String>) -> Self {
        Self {
            timestamp: h.timestamp,
            timestamp_ns: h.timestamp_ns.to_string(),
            project: slugs.get(&h.project_id).cloned().unwrap_or_default(),
            source: h.source,
            level: h.level,
            service: h.service,
            message: h.message,
            trace_id: h.trace_id,
            span_id: h.span_id,
            attributes: h.attributes,
        }
    }
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct Logs {
    /// Which logs these are, in words.
    pub scope: String,
    /// Oldest first.
    pub logs: Vec<LogLine>,
}

async fn logs(
    State(reads): State<Arc<Reads>>,
    Path(id): Path<String>,
    uri: Uri,
) -> Result<Response, Refusal> {
    let q = Query::parse(&uri, &["event"])?;
    let id = issue_id(&id)?;
    let event = q.event()?;
    let permit = reads.search_permit()?;
    let (moment, slugs) = reads
        .run(move |c| {
            of_issue(c, id, || {
                Ok((issues::moment(c, id, event.as_deref())?, issues::slugs(c)?))
            })
        })
        .await?
        .map_err(not_found)?;
    let moment = moment.ok_or_else(|| not_found("no such event of this issue"))?;
    let dir = crate::logs::dir(&reads.data);
    let logs = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let at_ns = moment.at.saturating_mul(1_000_000);
        let (scope, query) =
            context::logs_around(moment.project, at_ns, moment.trace_id.as_deref());
        let logs = search(&dir, &query)?
            .into_iter()
            .map(|h| LogLine::new(h, &slugs))
            .collect();
        anyhow::Ok(Logs { scope, logs })
    })
    .await
    .map_err(internal)?
    .map_err(internal)?;
    json(&logs)
}

/// A new status for up to [`issues::MAX_BULK`] issues. `release` goes only
/// with `resolved`: the release that contains the fix.
#[derive(Debug, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct StatusChange {
    pub issues: Vec<i64>,
    pub status: Status,
    #[serde(default)]
    #[cfg_attr(test, ts(optional))]
    pub release: Option<String>,
}

async fn set_status(State(reads): State<Arc<Reads>>, body: Body) -> Result<Response, Refusal> {
    let mut held = reads.budget.hold();
    let bytes = body::read_at_most(body, MAX_STATUS_BODY, &mut held)
        .await
        .map_err(priced)?;
    let change: StatusChange =
        serde_json::from_slice(&bytes).map_err(|e| Refusal::bad(e.to_string()))?;
    let issues = IssueIds::new(change.issues).map_err(Refusal::bad)?;
    let release = change.release.filter(|r| !r.is_empty());
    if release.as_ref().is_some_and(|r| r.len() > MAX_RELEASE) {
        let why = format!("a release has at most {MAX_RELEASE} bytes");
        return Err(Refusal::bad(why));
    }
    let status = match (change.status, release) {
        (Status::Resolved, in_release) => IssueStatus::Resolved {
            in_release,
            at: time::now_ms(),
        },
        (_, Some(_)) => return Err(Refusal::bad("a release goes only with resolved")),
        (Status::Unresolved, None) => IssueStatus::Unresolved,
        (Status::Ignored, None) => IssueStatus::Ignored,
    };
    let job = Job::SetStatus { issues, status };
    submit(&reads.writes, job, |why| why.starts_with("no issue "))
        .await?
        .map_err(not_found)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
