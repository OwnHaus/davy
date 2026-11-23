//! The [`Accounts`] of a self-hosted Davy: a session cookie looked up in the
//! SQLite tables, and what login needs to stay cheap under attack.

use super::Refusal;
use crate::{
    accounts::{
        Accounts, Person, Secret,
        backoff::{self, Backoff, Target},
        store::{self, Change},
    },
    http::internal,
    store::{Job, Sender, Write},
    time,
};
use axum::http::{HeaderMap, HeaderValue, header};
use rusqlite::Connection;
use std::{
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub const COOKIE: &str = "__Host-davy_session";
/// Argon2 runs at once at most this often, so logins cannot take every
/// blocking thread or more than this many times [`crate::accounts::HASH_MEMORY`].
pub const HASHES_AT_ONCE: usize = 2;
/// Requests that may wait for a hash slot, in the order they came, and for
/// how long. Logins that arrive together then wait their turn, and a flood
/// has to keep this many waiting to refuse anyone.
pub const HASH_QUEUE: usize = 32;
pub const HASH_WAIT: Duration = Duration::from_secs(3);
const TOO_MANY: &str = "too many attempts for this e-mail, wait";

/// Which count an attempt to guess a password goes to. A login and a
/// password change in an open session count apart, so failed logins for an
/// e-mail do not keep its person from changing their password.
#[derive(Clone, Copy)]
pub(super) enum Guess {
    Login,
    Change,
}

pub struct Local {
    db: Mutex<Connection>,
    /// The writer's UI queue.
    pub(super) writes: Sender,
    logins: Mutex<Backoff>,
    changes: Mutex<Backoff>,
    /// At most [`HASHES_AT_ONCE`] hashes, so they never hold more memory
    /// than that many times [`crate::accounts::HASH_MEMORY`].
    pub(super) hashing: Arc<Semaphore>,
    pub(super) queue: Semaphore,
}

/// The right to run one Argon2 hash.
pub(super) struct Slot {
    _permit: OwnedSemaphorePermit,
}

impl Local {
    /// `db` reads the account tables and `writes` is
    /// [`crate::store::Writer::ui`].
    pub fn new(db: Connection, writes: Sender) -> Self {
        Self {
            db: Mutex::new(db),
            writes,
            logins: Mutex::new(Backoff::new(backoff::CAPACITY)),
            changes: Mutex::new(Backoff::new(backoff::CAPACITY)),
            hashing: Arc::new(Semaphore::new(HASHES_AT_ONCE)),
            queue: Semaphore::new(HASH_QUEUE),
        }
    }

    pub(super) fn db(&self) -> Result<MutexGuard<'_, Connection>, Refusal> {
        self.db.lock().map_err(|e| internal(e).into())
    }

    /// A hash slot, after the requests that waited before this one, or `429`
    /// when [`HASH_QUEUE`] wait already or none frees in [`HASH_WAIT`].
    pub(super) async fn slot(&self) -> Result<Slot, Refusal> {
        let busy = || Refusal::wait(Duration::from_secs(1), "too many logins at once, try again");
        let _waiting = self.queue.try_acquire().map_err(|_full| busy())?;
        let permit = tokio::time::timeout(HASH_WAIT, Arc::clone(&self.hashing).acquire_owned())
            .await
            .map_err(|_late| busy())?
            .map_err(internal)?;
        Ok(Slot { _permit: permit })
    }

    fn backoff(&self, guess: Guess) -> &Mutex<Backoff> {
        match guess {
            Guess::Login => &self.logins,
            Guess::Change => &self.changes,
        }
    }

    /// Refuses a guess at `target` that is still in its wait, without
    /// counting it. [`Self::attempt`] counts it later, once it has a slot.
    pub(super) fn check(&self, guess: Guess, target: Target<'_>) -> Result<(), Refusal> {
        let backoff = self.backoff(guess).lock().map_err(internal)?;
        match backoff.waiting(target, Instant::now()) {
            Some(wait) => Err(Refusal::wait(wait, TOO_MANY)),
            None => Ok(()),
        }
    }

    /// Counts an attempt to guess the password of `target`, or says how
    /// long to wait.
    pub(super) fn attempt(&self, guess: Guess, target: Target<'_>) -> Result<(), Refusal> {
        let mut backoff = self.backoff(guess).lock().map_err(internal)?;
        backoff
            .attempt(target, Instant::now())
            .map_err(|wait| Refusal::wait(wait, TOO_MANY))
    }

    pub(super) fn succeeded(&self, guess: Guess, user: i64) {
        if let Ok(mut backoff) = self.backoff(guess).lock() {
            backoff.succeeded(user);
        }
    }
}

impl Accounts for Local {
    fn person(&self, headers: &HeaderMap) -> anyhow::Result<Option<Person>> {
        let Some(secret) = secret(headers) else {
            return Ok(None);
        };
        let hash = secret.hash();
        let now = time::now_ms();
        let session = {
            let db = self.db.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
            store::session(&db, &hash, now)?
        };
        Ok(session.map(|s| {
            if s.stale {
                let (done, _) = tokio::sync::oneshot::channel();
                let job = Job::Account(Change::Touch {
                    session: hash,
                    at: now,
                });
                // A full queue skips this touch, and the next request tries again.
                drop(self.writes.try_send(Write { job, done }));
            }
            s.person
        }))
    }
}

/// The session secret in the request's cookie, if it has a well-formed one.
pub fn secret(headers: &HeaderMap) -> Option<Secret> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == COOKIE)
        .and_then(|(_, value)| Secret::parse(value))
}

/// The cookie that holds `secret` until the session can end at the latest.
pub fn cookie(secret: &Secret) -> Result<HeaderValue, Refusal> {
    let max_age = crate::accounts::SESSION_MAX_MS / 1000;
    let value = format!(
        "{COOKIE}={}; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age={max_age}",
        secret.encode()
    );
    HeaderValue::try_from(value).map_err(|e| internal(e).into())
}

/// The cookie that makes the browser forget the session.
pub fn forget() -> HeaderValue {
    HeaderValue::from_static(
        "__Host-davy_session=; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age=0",
    )
}
