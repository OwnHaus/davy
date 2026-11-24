//! Where a project's policy is kept, and who may change it.
//!
//! The CLI may set any policy: it needs the data directory. The HTTP API
//! takes only changes that remove more, so an agent with the admin token can
//! tighten scrubbing but not weaken it.
use super::{Missing, Policy};
use crate::{
    http::{Failure, bad, internal, json_response},
    store,
    token::{self, Token},
};
use anyhow::Context;
use axum::{
    Router,
    body::Body,
    extract::{Path, State},
    http::StatusCode,
    middleware,
    response::Response,
    routing::get,
};
use rusqlite::{Connection, OptionalExtension};
use std::{
    collections::HashMap,
    io::Write,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

/// How long a policy read from the database is used before it is read
/// again, so a change from the CLI applies within this without a restart.
pub(super) const FRESH: Duration = Duration::from_secs(2);

/// The stored policy of `project`, if the project exists.
pub fn stored(c: &Connection, project: i64) -> anyhow::Result<Option<Policy>> {
    let json: Option<String> = c
        .query_row("SELECT scrub FROM projects WHERE id=?1", [project], |r| {
            r.get(0)
        })
        .optional()?;
    json.map(|json| serde_json::from_str(&json).context("stored scrub policy"))
        .transpose()
}

pub fn read(c: &Connection, project: i64) -> anyhow::Result<Policy> {
    stored(c, project)?.with_context(|| format!("no project {project}"))
}

/// Stores `policy`. False when there is no such project.
pub fn set(c: &Connection, project: i64, policy: &Policy) -> anyhow::Result<bool> {
    let json = serde_json::to_string(policy)?;
    let changed = c.execute(
        "UPDATE projects SET scrub=?1 WHERE id=?2",
        rusqlite::params![json, project],
    )?;
    Ok(changed == 1)
}

/// Writes a change of policy to the server's log.
#[expect(
    clippy::print_stderr,
    reason = "the server has no logger yet, stderr is its log"
)]
fn log_change(project: &str, by: &str, old: &Policy, new: &Policy) {
    let json = |p: &Policy| serde_json::to_string(p).unwrap_or_default();
    eprintln!(
        "scrub: warning: project {project} policy changed {by}: {} -> {}",
        json(old),
        json(new)
    );
}

/// Policies by project, each read again after [`FRESH`].
#[derive(Default)]
pub struct Cache {
    entries: Mutex<HashMap<i64, (Instant, Arc<Policy>)>>,
}

impl Cache {
    /// The policy of `project`, from `db` when the one held is stale. A
    /// policy that changed since, not through the API, is logged.
    pub fn get(&self, db: &Mutex<Connection>, project: i64) -> anyhow::Result<Arc<Policy>> {
        self.get_at(db, project, Instant::now())
    }

    /// [`Cache::get`] at `now`.
    pub(super) fn get_at(
        &self,
        db: &Mutex<Connection>,
        project: i64,
        now: Instant,
    ) -> anyhow::Result<Arc<Policy>> {
        let mut entries = self.entries.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        if let Some((at, policy)) = entries.get(&project)
            && now.duration_since(*at) < FRESH
        {
            return Ok(Arc::clone(policy));
        }
        let fresh = {
            let c = db.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
            Arc::new(read(&c, project)?)
        };
        if let Some((_, old)) = entries.get(&project)
            && **old != *fresh
        {
            log_change(&project.to_string(), "outside the API", old, &fresh);
        }
        entries.insert(project, (now, Arc::clone(&fresh)));
        Ok(fresh)
    }

    /// Holds `policy` for `project` as just read, after the API stored it.
    fn put(&self, project: i64, policy: Policy) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.insert(project, (Instant::now(), Arc::new(policy)));
        }
    }
}

#[derive(clap::Args)]
pub struct Args {
    /// Project slug to configure.
    pub slug: String,
    #[arg(long)]
    pub data: PathBuf,
    /// Replace the policy with a JSON object. Fields it leaves out take
    /// their defaults. Omit to read it.
    #[arg(long)]
    pub settings: Option<String>,
}
pub fn run(out: &mut impl Write, args: &Args) -> anyhow::Result<()> {
    let c = store::open(&args.data)?;
    let id = store::project_by_slug(&c, &args.slug)?;
    if let Some(json) = &args.settings {
        let new = super::parse(json.as_bytes(), Missing::Default)?;
        let old = read(&c, id)?;
        set(&c, id, &new)?;
        if old != new {
            log_change(&args.slug, "by the CLI", &old, &new);
        }
    }
    writeln!(out, "{}", serde_json::to_string(&read(&c, id)?)?)?;
    Ok(())
}
#[derive(Clone)]
struct Api {
    db: Arc<Mutex<Connection>>,
    sender: store::Sender,
    policies: Arc<Cache>,
    /// Held from reading the old policy until the new one is stored, so two
    /// changes that each tighten cannot together weaken it.
    changing: Arc<tokio::sync::Mutex<()>>,
}
pub fn router(c: Connection, sender: store::Sender, token: Token, policies: Arc<Cache>) -> Router {
    Router::new()
        .route("/api/projects/{project}/scrub", get(show).put(update))
        .with_state(Api {
            db: Arc::new(Mutex::new(c)),
            sender,
            policies,
            changing: Arc::default(),
        })
        .layer(middleware::from_fn_with_state(
            Arc::new(token),
            token::require,
        ))
}
fn not_found(project: i64) -> Failure {
    (StatusCode::NOT_FOUND, format!("no project {project}"))
}
fn current(api: &Api, project: i64) -> Result<Policy, Failure> {
    let c = api.db.lock().map_err(internal)?;
    stored(&c, project)
        .map_err(internal)?
        .ok_or_else(|| not_found(project))
}
fn respond(policy: &Policy) -> Result<Response, Failure> {
    Ok(json_response(
        &serde_json::to_value(policy).map_err(internal)?,
    ))
}
async fn show(State(api): State<Api>, Path(id): Path<i64>) -> Result<Response, Failure> {
    respond(&current(&api, id)?)
}
async fn update(
    State(api): State<Api>,
    Path(id): Path<i64>,
    body: Body,
) -> Result<Response, Failure> {
    let bytes = tokio::time::timeout(
        Duration::from_secs(5),
        axum::body::to_bytes(body, 16 * 1024),
    )
    .await
    .map_err(bad)?
    .map_err(bad)?;
    let new = super::parse(&bytes, Missing::Refuse).map_err(bad)?;
    let _changing = api.changing.lock().await;
    let old = current(&api, id)?;
    let lost = old.weakened(&new);
    if !lost.is_empty() {
        return Err((
            StatusCode::FORBIDDEN,
            format!(
                "the API only tightens scrubbing, and this policy {}. To weaken it, run \
                 `davy project scrub <slug> --data <dir> --settings '<json>'` where the \
                 data directory is",
                lost.join(", ")
            ),
        ));
    }
    if old != new {
        let job = store::Job::Scrub {
            project: id,
            policy: new.clone(),
        };
        store::submit(&api.sender, job).await?;
        log_change(&id.to_string(), "by the API", &old, &new);
        api.policies.put(id, new.clone());
    }
    respond(&new)
}
