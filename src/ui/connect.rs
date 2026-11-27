//! What the Projects and Setup pages show: where each SDK sends, and which
//! steps of the setup are done.

use super::{
    Refusal,
    issues::{Query, Reads},
    json, logs,
};
use crate::{
    event::Timestamp,
    firsts::{self, First},
    install,
    logs::store::{LogQuery, search},
    store::{self, Job},
    time::{now_ns, rfc3339_ms},
};
use axum::{Router, extract::State, http::Uri, response::Response, routing::get};
use serde::Serialize;
use std::sync::Arc;

pub fn routes(reads: Arc<Reads>, public_url: &url::Url) -> Router {
    let state = Arc::new(Connect {
        reads,
        public_url: public_url.clone(),
    });
    Router::new()
        .route("/ui/api/connect", get(connect))
        .route("/ui/api/setup", get(setup))
        .with_state(state)
}

struct Connect {
    reads: Arc<Reads>,
    public_url: url::Url,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct ProjectKey {
    pub slug: String,
    /// What a Sentry SDK takes.
    pub dsn: String,
    /// The public key in the DSN, which log shippers send as a bearer token.
    pub key: String,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct Endpoints {
    pub public_url: String,
    /// Where log shippers POST JSON or NDJSON.
    pub logs: String,
    /// Where an OpenTelemetry exporter POSTs logs over HTTP.
    pub otlp: String,
    /// Where agents connect, when the server has an admin token.
    pub mcp: String,
    pub projects: Vec<ProjectKey>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct Step {
    pub what: First,
    pub required: bool,
    /// When it was first done, or None while it is not.
    pub at: Option<String>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct Setup {
    pub steps: Vec<Step>,
}

fn at_path(url: &url::Url, path: &str) -> String {
    let mut u = url.clone();
    u.set_path(path);
    u.into()
}

async fn connect(State(s): State<Arc<Connect>>, uri: Uri) -> Result<Response, Refusal> {
    Query::parse(&uri, &[])?;
    let keys = s
        .reads
        .run(|c| {
            let mut q = c.prepare("SELECT id,slug,public_key FROM projects ORDER BY slug")?;
            let rows = q.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get(1)?, r.get(2)?)))?;
            Ok(rows.collect::<rusqlite::Result<Vec<(i64, String, String)>>>()?)
        })
        .await?;
    let projects = keys
        .into_iter()
        .map(|(id, slug, key)| {
            Ok(ProjectKey {
                slug,
                dsn: install::dsn(&s.public_url, &key, id)?.into(),
                key,
            })
        })
        .collect::<anyhow::Result<_>>()?;
    json(&Endpoints {
        public_url: s.public_url.as_str().trim_end_matches('/').to_owned(),
        logs: at_path(&s.public_url, "/"),
        otlp: at_path(&s.public_url, "/v1/logs"),
        mcp: at_path(&s.public_url, "/mcp"),
        projects,
    })
}

async fn setup(State(s): State<Arc<Connect>>, uri: Uri) -> Result<Response, Refusal> {
    Query::parse(&uri, &[])?;
    let mut steps = s.reads.run(firsts::all).await?;
    if let Some(log) = steps
        .iter_mut()
        .find(|(what, at, _)| *what == First::Log && at.is_none())
    {
        log.1 = any_log(&s.reads).await?;
        log.2 = log.1.is_some();
    }
    // A step found in its rows is written down once, so it stays done when
    // retention deletes them. Later polls read it from firsts.
    for &(what, at, found) in &steps {
        if let (Some(at), true) = (at, found) {
            store::queue_and_wait(s.reads.writes(), Job::First { what, at }).await;
        }
    }
    json(&Setup {
        steps: steps
            .into_iter()
            .map(|(what, at, _)| Step {
                what,
                required: what.required(),
                at: at.map(rfc3339_ms),
            })
            .collect(),
    })
}

/// The time of the newest log of a project a person made, in milliseconds.
/// Davy's own lines are not a step a person took.
async fn any_log(reads: &Arc<Reads>) -> Result<Option<Timestamp>, Refusal> {
    let projects = reads
        .run(|c| {
            let mut q = c.prepare("SELECT id FROM projects WHERE own=0")?;
            let rows = q.query_map([], |r| r.get::<_, i64>(0))?;
            Ok(rows.collect::<rusqlite::Result<Vec<i64>>>()?)
        })
        .await?;
    let until = now_ns();
    let newest = logs::searching(reads, move |dir| {
        let mut newest = None;
        for project in projects {
            let query = LogQuery {
                project: Some(project),
                since: 0,
                until,
                min_level: None,
                text: None,
                pattern: None,
                limit: 1,
                skip: 0,
            };
            // No cold note: with no text, the first block read holds the
            // newest log, so the budget never cuts this search short.
            let at = search(dir, &query)?.first().map(|h| h.timestamp_ns);
            newest = newest.max(at);
        }
        Ok(newest)
    })
    .await?;
    Ok(newest.map(|ns| ns.div_euclid(1_000_000)))
}
