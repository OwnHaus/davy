//! Login, logout, the welcome link, and a person's own password and
//! sessions.

use super::{
    Refusal, json,
    session::{self, Guess, Local},
    write,
};
use crate::{
    accounts::{
        Email, Person, Secret, check_new_password, hash_password,
        store::{self, Change},
        verify_password,
    },
    http::{body, internal},
    time,
};
use axum::{
    Extension, Router,
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::sync::Arc;

/// What a login, welcome or password body may weigh.
pub const MAX_BODY: usize = 4 * 1024;
const WRONG: &str = "wrong e-mail or password";
const LINK_GONE: &str = "this link was used or is older than 24 hours, ask for a new one";

/// Who is logged in.
#[derive(Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct Me {
    pub email: String,
}

/// Who logged in, and the page under `/ui/` to go to.
#[derive(Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub(super) struct LoggedIn {
    email: String,
    next: String,
}

/// Login, logout and welcome, which need no session.
pub fn open(local: Arc<Local>) -> Router {
    Router::new()
        .route("/ui/api/login", post(login))
        .route("/ui/api/logout", post(logout))
        .route("/ui/api/welcome", post(welcome))
        .with_state(local)
}

/// A person's own account, behind the session check.
pub fn protected(local: Arc<Local>) -> Router {
    Router::new()
        .route("/ui/api/account/password", post(set_password))
        .route("/ui/api/account/logout-others", post(logout_others))
        .with_state(local)
}

async fn read<T: DeserializeOwned>(local: &Local, body: Body) -> Result<T, Refusal> {
    let mut held = local.budget().hold();
    let bytes = body::read_at_most(body, MAX_BODY, &mut held).await?;
    serde_json::from_slice(&bytes).map_err(|e| Refusal::bad(e.to_string()))
}

async fn hashing<T: Send + 'static>(
    slot: session::Slot,
    f: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> Result<T, Refusal> {
    let run = move || {
        let _slot = slot;
        f()
    };
    Ok(tokio::task::spawn_blocking(run).await.map_err(internal)??)
}

fn signed_in(secret: &Secret, body: &impl Serialize) -> Result<Response, Refusal> {
    Ok((
        [(header::SET_COOKIE, session::cookie(secret)?)],
        json(body)?,
    )
        .into_response())
}

#[derive(Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub(super) struct Login {
    email: String,
    password: String,
    #[cfg_attr(test, ts(optional))]
    next: Option<String>,
}

async fn login(State(local): State<Arc<Local>>, body: Body) -> Result<Response, Refusal> {
    let req: Login = read(&local, body).await?;
    let email = Email::parse(&req.email).map_err(Refusal::bad)?;
    let slot = local.slot()?;
    local.attempt(Guess::Login, &email)?;
    let found = store::password(&*local.db()?, &email)?;
    let (user, hash) = found.map_or((None, None), |(id, hash)| (Some(id), hash));
    let checked = hash.clone();
    let password = req.password;
    let matched = hashing(slot, move || {
        Ok(verify_password(checked.as_deref(), &password))
    })
    .await?;
    let (true, Some(user), Some(hash)) = (matched, user, hash) else {
        return Err(Refusal::new(StatusCode::UNAUTHORIZED, WRONG));
    };
    let secret = Secret::random().map_err(internal)?;
    let change = Change::Login {
        user,
        password: hash,
        session: secret.hash(),
        at: time::now_ms(),
    };
    write(&local.writes, change)
        .await?
        .map_err(|_password_changed| Refusal::new(StatusCode::UNAUTHORIZED, WRONG))?;
    local.succeeded(Guess::Login, &email);
    let next = next_page(req.next.as_deref());
    let body = LoggedIn {
        email: email.as_str().to_owned(),
        next,
    };
    signed_in(&secret, &body)
}

/// `next` when it is a path under `/ui/`, else `/ui/`, so a login link
/// cannot send anyone to another site.
pub fn next_page(next: Option<&str>) -> String {
    const HOME: &str = "/ui/";
    const BASE: &str = "http://davy.invalid/";
    let Some(next) = next else {
        return HOME.to_owned();
    };
    let joined = url::Url::parse(BASE).and_then(|base| base.join(next));
    match joined {
        Ok(url) if url.as_str().starts_with("http://davy.invalid/ui/") => {
            url[url::Position::BeforePath..].to_owned()
        }
        _ => HOME.to_owned(),
    }
}

async fn logout(State(local): State<Arc<Local>>, headers: HeaderMap) -> Result<Response, Refusal> {
    // Only a session that exists costs a write, so a flood of made-up
    // cookies does not fill the writer's UI queue.
    let known = match session::secret(&headers) {
        Some(secret) => {
            let session = secret.hash();
            let found = store::session(&*local.db()?, &session, time::now_ms())?;
            found.map(|_| session)
        }
        None => None,
    };
    if let Some(session) = known {
        write(&local.writes, Change::Logout { session })
            .await?
            .map_err(internal)?;
    }
    Ok((
        StatusCode::NO_CONTENT,
        [(header::SET_COOKIE, session::forget())],
    )
        .into_response())
}

#[derive(Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub(super) struct Welcome {
    secret: String,
    password: String,
}

async fn welcome(State(local): State<Arc<Local>>, body: Body) -> Result<Response, Refusal> {
    let gone = || Refusal::new(StatusCode::GONE, LINK_GONE);
    let req: Welcome = read(&local, body).await?;
    let invite = Secret::parse(&req.secret).ok_or_else(gone)?.hash();
    check_new_password(&req.password).map_err(Refusal::bad)?;
    let now = time::now_ms();
    if !store::invite_works(&*local.db()?, &invite, now)? {
        return Err(gone());
    }
    let slot = local.slot()?;
    let password = req.password;
    let hash = hashing(slot, move || hash_password(&password)).await?;
    let secret = Secret::random().map_err(internal)?;
    let change = Change::Welcome {
        invite,
        password: hash,
        session: secret.hash(),
        at: now,
    };
    write(&local.writes, change)
        .await?
        .map_err(|_link_used| gone())?;
    let session = store::session(&*local.db()?, &secret.hash(), now)?;
    let email = session
        .map(|s| s.person.email.to_string())
        .unwrap_or_default();
    signed_in(&secret, &Me { email })
}

/// The session the request came with. The session check ran first.
fn current(headers: &HeaderMap) -> Result<Secret, Refusal> {
    session::secret(headers).ok_or_else(|| Refusal::new(StatusCode::UNAUTHORIZED, "log in first"))
}

#[derive(Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub(super) struct NewPassword {
    current: String,
    new: String,
}

async fn set_password(
    State(local): State<Arc<Local>>,
    Extension(person): Extension<Person>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, Refusal> {
    let keep = current(&headers)?.hash();
    let req: NewPassword = read(&local, body).await?;
    check_new_password(&req.new).map_err(Refusal::bad)?;
    let slot = local.slot()?;
    local.attempt(Guess::Change, &person.email)?;
    let old = store::password_of(&*local.db()?, person.id)?;
    let checked = old.clone();
    let new = hashing(slot, move || {
        if !verify_password(checked.as_deref(), &req.current) {
            return Ok(None);
        }
        hash_password(&req.new).map(Some)
    })
    .await?;
    let (Some(old), Some(new)) = (old, new) else {
        return Err(Refusal::bad("the current password is wrong"));
    };
    let change = Change::SetPassword {
        user: person.id,
        old,
        new,
        keep,
    };
    write(&local.writes, change)
        .await?
        .map_err(|_password_changed| {
            Refusal::new(
                StatusCode::CONFLICT,
                "the password changed meanwhile, try again",
            )
        })?;
    local.succeeded(Guess::Change, &person.email);
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn logout_others(
    State(local): State<Arc<Local>>,
    Extension(person): Extension<Person>,
    headers: HeaderMap,
) -> Result<Response, Refusal> {
    let change = Change::LogoutOthers {
        user: person.id,
        keep: current(&headers)?.hash(),
    };
    write(&local.writes, change).await?.map_err(internal)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
