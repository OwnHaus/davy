//! The Agents pages: the agent tokens a person changes, and the ledger of
//! what agents and people changed, newest first.

use super::{Refusal, issues::Query, issues::Reads, json, submit};
use crate::{
    access::Level,
    accounts::Person,
    http::body,
    ledger::{self, Row, describe},
    store::Job,
    time,
    tokens::{self, Change, Name},
};
use axum::{
    Extension, Router,
    body::Body,
    extract::State,
    http::{StatusCode, Uri},
    response::Response,
    routing::get,
};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Ledger rows on a page.
pub const PAGE: usize = 50;
/// A change names one token and a level, far below this.
const MAX_BODY: usize = 1024;

pub fn routes(reads: Arc<Reads>) -> Router {
    Router::new()
        .route("/ui/api/tokens", get(list).post(change))
        .route("/ui/api/ledger", get(rows))
        .with_state(reads)
}

/// A person's change to one agent token. A token is added with `davy
/// token add`, which prints its secret once, so the web UI never holds one.
#[derive(Debug, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(tag = "do", rename_all = "lowercase")]
pub enum TokenChange {
    Level { name: String, level: Level },
    Unlock { name: String },
    Lock { name: String },
    Revoke { name: String },
}

/// A page of the ledger.
#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct LedgerPage {
    pub rows: Vec<Row>,
    /// Every actor with a row, for the filter.
    pub actors: Vec<String>,
    /// The `before` of the next, older page, when there is one.
    pub next_before: Option<i64>,
}

/// Every token, live ones first.
async fn list(State(reads): State<Arc<Reads>>, uri: Uri) -> Result<Response, Refusal> {
    Query::parse(&uri, &[])?;
    json(&reads.run(tokens::list).await?)
}

/// Stores a [`TokenChange`] with its ledger row and answers with every
/// token as it is now.
async fn change(
    State(reads): State<Arc<Reads>>,
    Extension(person): Extension<Person>,
    body: Body,
) -> Result<Response, Refusal> {
    let bytes = body::read_small(body, MAX_BODY, super::account::BODY_TIMEOUT).await?;
    let asked: TokenChange =
        serde_json::from_slice(&bytes).map_err(|e| Refusal::bad(e.to_string()))?;
    let change = token_change(asked, person.email.as_str(), time::now_ms())?;
    let name = token_name(&change).to_owned();
    let (unknown, low) = (tokens::unknown(&name), format!("token {name:?} has level "));
    let what = describe::token(&change);
    let job = super::recorded(
        &person,
        describe::token_action(&change),
        what,
        Job::Token(change),
    );
    let known = |why: &str| why == unknown || why.starts_with(&low);
    if let Err(why) = submit(reads.writes(), job, known).await? {
        let status = if why == unknown {
            StatusCode::NOT_FOUND
        } else {
            StatusCode::CONFLICT
        };
        return Err(Refusal::new(status, why));
    }
    json(&reads.run(tokens::list).await?)
}

/// The writer's change for what a person asked, with the name checked.
fn token_change(asked: TokenChange, by: &str, at: i64) -> Result<Change, Refusal> {
    let checked = |name: String| {
        Name::parse(&name).map_err(Refusal::bad)?;
        Ok::<_, Refusal>(name)
    };
    Ok(match asked {
        TokenChange::Level { name, level } => Change::Level {
            name: checked(name)?,
            level,
        },
        TokenChange::Unlock { name } => Change::Unlock {
            name: checked(name)?,
            by: by.to_owned(),
            at,
        },
        TokenChange::Lock { name } => Change::Lock {
            name: checked(name)?,
        },
        TokenChange::Revoke { name } => Change::Revoke {
            name: checked(name)?,
            at,
        },
    })
}

fn token_name(change: &Change) -> &str {
    match change {
        Change::Level { name, .. }
        | Change::Unlock { name, .. }
        | Change::Lock { name }
        | Change::Revoke { name, .. } => name,
        Change::Used { .. } => "",
    }
}

/// The [`PAGE`] newest rows older than `before`, of `actor` when given.
async fn rows(State(reads): State<Arc<Reads>>, uri: Uri) -> Result<Response, Refusal> {
    let q = Query::parse(&uri, &["actor", "before"])?;
    let actor = q.owned("actor");
    let before = q
        .get("before")
        .map(|v| {
            v.parse::<i64>()
                .ok()
                .filter(|n| *n > 0)
                .ok_or(Refusal::bad("before is the next_before of the page before"))
        })
        .transpose()?;
    json(
        &reads
            .run(move |c| page(c, actor.as_deref(), before))
            .await?,
    )
}

pub fn page(
    c: &Connection,
    actor: Option<&str>,
    before: Option<i64>,
) -> anyhow::Result<LedgerPage> {
    let mut rows = ledger::list(c, actor, before, PAGE + 1)?;
    let next_before = (rows.len() > PAGE).then(|| {
        rows.truncate(PAGE);
        rows.last().map(|r| r.id)
    });
    Ok(LedgerPage {
        rows,
        actors: ledger::actors(c)?,
        next_before: next_before.flatten(),
    })
}
