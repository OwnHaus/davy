//! The usage page: the report and savings of `davy usage --json`, and what
//! each project stored and what became of what it sent, per day.

use super::{Refusal, issues::Query, issues::Reads, json};
use crate::{
    query,
    retention::{self, Defaults},
    store, time,
    usage::{self, savings},
};
use axum::{
    Router,
    extract::State,
    http::{StatusCode, Uri},
    response::Response,
    routing::get,
};
use std::{path::Path, sync::Arc, time::Duration};

pub fn routes(reads: Arc<Reads>, defaults: Defaults) -> Router {
    Router::new()
        .route("/ui/api/usage", get(report))
        .route("/ui/api/usage/days", get(days))
        .with_state(Arc::new(Usage { reads, defaults }))
}

struct Usage {
    reads: Arc<Reads>,
    defaults: Defaults,
}

/// What `davy usage --json` prints for the same `view`, `project`, `since`
/// and `top`: sizes in bytes. The disk is the walk the MCP tool `usage`
/// shares, at most a minute old. Past [`usage::Shared`]'s reads at once it
/// answers 429.
async fn report(State(s): State<Arc<Usage>>, uri: Uri) -> Result<Response, Refusal> {
    let _permit = permit(&s)?;
    let q = Query::parse(&uri, &["view", "project", "since", "top"])?;
    let view = match q.get("view") {
        None | Some("summary") => savings::View::Summary,
        Some("savings") => savings::View::Savings,
        Some(_) => return Err(Refusal::bad("view is summary or savings")),
    };
    let top = match q.get("top") {
        None => savings::DEFAULT_TOP,
        Some(top) => top
            .parse()
            .ok()
            .filter(|top| (1..=savings::MAX_TOP).contains(top))
            .ok_or_else(|| Refusal::bad(format!("top is 1 to {}", savings::MAX_TOP)))?,
    };
    let now = time::now_ns();
    usage::window(q.get("since"), now).map_err(Refusal::bad)?;
    let request = usage::Request {
        project: q.owned("project"),
        since: q.owned("since"),
        what_if: usage::forecast::WhatIf::default(),
        defaults: s.defaults,
    };
    let wanted = savings::Request {
        project: request.project.clone(),
        top,
        defaults: s.defaults,
    };
    let prepared = read(&s.reads, move |c, _| {
        known(c, request.project.as_deref())?;
        Ok(usage::prepare(c, &request, now)?)
    })
    .await?;
    let data: Arc<Path> = Arc::from(s.reads.data());
    let disk = s.reads.usage().measure(data, prepared.today).await;
    let report = prepared.finish(disk);
    if view == savings::View::Summary {
        return json(&report.json(false));
    }
    let found = read(&s.reads, move |c, data| {
        Ok(savings::savings(c, data, &report, &wanted, now)?)
    })
    .await?;
    json(&found.json(false))
}

/// Per UTC day of the window `since` names, 30 days back without one, what
/// each project, or the one named `project`, stored, and what became of
/// what it sent. It reads the counting tables only and walks no disk, and
/// names at most [`usage::days::NAMED`] projects. Past
/// [`usage::Shared`]'s reads at once it answers 429.
async fn days(State(s): State<Arc<Usage>>, uri: Uri) -> Result<Response, Refusal> {
    let _permit = permit(&s)?;
    let q = Query::parse(&uri, &["project", "since"])?;
    let now = time::now_ns();
    let since = usage::window(Some(q.get("since").unwrap_or("30d")), now).map_err(Refusal::bad)?;
    let project = q.owned("project");
    json(
        &read(&s.reads, move |c, _| {
            known(c, project.as_deref())?;
            Ok(usage::days::days(c, project.as_deref(), since, now)?)
        })
        .await?,
    )
}

/// A place among the usage reads that may run at once, the MCP tool's
/// included, or a 429.
fn permit(s: &Usage) -> Result<tokio::sync::OwnedSemaphorePermit, Refusal> {
    s.reads
        .usage()
        .permit()
        .ok_or_else(|| Refusal::wait(Duration::from_secs(1), usage::BUSY))
}

/// Refuses a project no one made, by name.
fn known(c: &rusqlite::Connection, project: Option<&str>) -> Result<(), Refusal> {
    match project {
        Some(slug) if store::project_id(c, slug)?.is_none() => Err(Refusal::new(
            StatusCode::NOT_FOUND,
            retention::unknown(slug),
        )),
        _ => Ok(()),
    }
}

/// Runs `f` on a read-only connection of its own, off the async threads,
/// as the MCP tool does: the savings read log indexes, which takes longer
/// than the other routes hold the shared connection.
async fn read<T: Send + 'static>(
    reads: &Reads,
    f: impl FnOnce(&rusqlite::Connection, &Path) -> Result<T, Refusal> + Send + 'static,
) -> Result<T, Refusal> {
    let data = reads.data().to_owned();
    tokio::task::spawn_blocking(move || f(&query::open(&data)?, &data))
        .await
        .map_err(crate::http::internal)?
}
