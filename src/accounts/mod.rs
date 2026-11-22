//! The people who use the web UI: their e-mails, passwords, sessions and the
//! links that let them set a password. The tables are created by the
//! migrations in `crate::store`.

pub mod backoff;
pub mod cli;
pub mod store;
#[cfg(test)]
mod tests;

use crate::event::Timestamp;
use argon2::{
    Algorithm, Argon2, Params, Version,
    password_hash::{PasswordHasher as _, PasswordVerifier as _},
};
use base64::Engine as _;

const DAY_MS: Timestamp = 24 * 60 * 60 * 1000;
/// A session ends this long after it was last used.
pub const SESSION_IDLE_MS: Timestamp = 14 * DAY_MS;
/// A session ends this long after the login, used or not.
pub const SESSION_MAX_MS: Timestamp = 90 * DAY_MS;
/// A welcome link works this long, and once.
pub const INVITE_MS: Timestamp = DAY_MS;
/// How often a session's last use is written down.
pub const TOUCH_EVERY_MS: Timestamp = 60 * 60 * 1000;

pub const MIN_PASSWORD_CHARS: usize = 12;
pub const MAX_PASSWORD_BYTES: usize = 1024;
/// What one Argon2 hash or check allocates.
pub const HASH_MEMORY: usize = Params::DEFAULT_M_COST as usize * 1024;

/// A person signed in to the web UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Person {
    pub id: i64,
    pub email: Email,
}

/// Turns a request into the person who sent it. This crate implements it on
/// the session cookie and the SQLite tables; the cloud brings its own.
pub trait Accounts: Send + Sync + 'static {
    /// The person behind `headers`, or `None` without a live session.
    fn person(&self, headers: &axum::http::HeaderMap) -> anyhow::Result<Option<Person>>;
}

/// An e-mail address, trimmed and lowercased, so one person has one row.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Email(String);

impl Email {
    const MAX_BYTES: usize = 254;

    pub fn parse(s: &str) -> Result<Self, String> {
        let e = s.trim().to_lowercase();
        let shaped = e
            .split_once('@')
            .is_some_and(|(local, domain)| !local.is_empty() && domain.contains('.'));
        let clean = !e.chars().any(|c| c.is_whitespace() || c.is_control());
        if !shaped || !clean || e.len() > Self::MAX_BYTES || e.matches('@').count() != 1 {
            return Err(format!("{s:?} is not an e-mail address"));
        }
        Ok(Self(e))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Email {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Checks a password a person chooses.
pub fn check_new_password(p: &str) -> Result<(), String> {
    if p.chars().count() < MIN_PASSWORD_CHARS {
        return Err(format!(
            "a password needs at least {MIN_PASSWORD_CHARS} characters"
        ));
    }
    if p.len() > MAX_PASSWORD_BYTES {
        return Err(format!(
            "a password may have at most {MAX_PASSWORD_BYTES} bytes"
        ));
    }
    Ok(())
}

fn argon2() -> Argon2<'static> {
    Argon2::new(Algorithm::Argon2id, Version::V0x13, Params::default())
}

/// The PHC string of `password` with a fresh salt. Takes tens of
/// milliseconds and [`HASH_MEMORY`], so it runs on a blocking thread.
pub fn hash_password(password: &str) -> anyhow::Result<String> {
    Ok(argon2().hash_password(password.as_bytes())?.to_string())
}

/// A hash of a random password with the parameters of [`hash_password`].
/// Checking a login for an e-mail without a password costs the same as one
/// with a password, so the time does not tell which e-mails exist.
pub const DUMMY_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$9a7NHiQzqnIx0dxjLDudLA$lPb/fEKEdvnsMuXXDgZ1mJwF4wXGKo27QSUs+7C68/Y";

/// Whether `password` matches the PHC string `hash`. Without a hash it checks
/// against [`DUMMY_HASH`] and returns false.
pub fn verify_password(hash: Option<&str>, password: &str) -> bool {
    let matched = argon2()
        .verify_password(password.as_bytes(), hash.unwrap_or(DUMMY_HASH))
        .is_ok();
    matched && hash.is_some()
}

/// 32 random bytes given out once, in a cookie or a link. The database keeps
/// only their [`hash`](Secret::hash).
pub struct Secret([u8; 32]);

/// The BLAKE3 hash of a [`Secret`], the key of a session or an invite.
pub type SecretHash = [u8; 32];

impl Secret {
    pub fn random() -> Result<Self, getrandom::Error> {
        let mut bytes = [0_u8; 32];
        getrandom::fill(&mut bytes)?;
        Ok(Self(bytes))
    }

    /// A secret in the form [`encode`](Secret::encode) gives.
    pub fn parse(s: &str) -> Option<Self> {
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(s)
            .ok()?;
        Some(Self(bytes.try_into().ok()?))
    }

    /// 43 characters of URL-safe base64.
    pub fn encode(&self) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(self.0)
    }

    pub fn hash(&self) -> SecretHash {
        *blake3::hash(&self.0).as_bytes()
    }
}
