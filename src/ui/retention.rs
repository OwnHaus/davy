//! How long each project keeps its logs and errors, read and changed.

use super::{Refusal, issues::Query, issues::Reads, json, submit};
use crate::{
    http::body,
    retention::{self, By, Change, Defaults},
    store::Job,
    time,
};
use axum::{
    Router,
    body::Body,
    extract::State,
    http::{StatusCode, Uri},
    response::Response,
    routing::get,
};
use std::sync::Arc;

/// A change names one project and two settings, far below this.
const MAX_BODY: usize = 4 * 1024;

pub fn routes(reads: Arc<Reads>, defaults: Defaults) -> Router {
    Router::new()
        .route("/ui/api/retention", get(list).post(change))
        .with_state(Arc::new(Settings { reads, defaults }))
}

struct Settings {
    reads: Arc<Reads>,
    defaults: Defaults,
}

/// What every project keeps, by slug.
async fn list(State(s): State<Arc<Settings>>, uri: Uri) -> Result<Response, Refusal> {
    Query::parse(&uri, &[])?;
    let defaults = s.defaults;
    json(
        &s.reads
            .run(move |c| retention::all(c, defaults, time::now_ms()))
            .await?,
    )
}

/// Stores a [`Change`] and answers with what the project keeps now.
async fn change(State(s): State<Arc<Settings>>, body: Body) -> Result<Response, Refusal> {
    let bytes = body::read_small(body, MAX_BODY, super::account::BODY_TIMEOUT).await?;
    let change: Change = serde_json::from_slice(&bytes).map_err(|e| Refusal::bad(e.to_string()))?;
    change.check().map_err(Refusal::bad)?;
    let slug = change.project.clone();
    let unknown = retention::unknown(&slug);
    submit(
        s.reads.writes(),
        Job::SetRetention(change, By::Person),
        |why| why == unknown,
    )
    .await?
    .map_err(|why| Refusal::new(StatusCode::NOT_FOUND, why))?;
    let defaults = s.defaults;
    let kept = s
        .reads
        .run(move |c| retention::one(c, &slug, defaults, time::now_ms()))
        .await?;
    json(&kept.ok_or_else(|| Refusal::new(StatusCode::NOT_FOUND, "the project is gone"))?)
}
