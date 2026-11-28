//! Agent tokens: named bearer tokens for `/mcp`, each with a level (see
//! src/access.rs), kept only as their hash. `davy token add` makes one and
//! prints its secret once; a person changes its level, unlocks destructive
//! changes for it or revokes it in the web UI or with `davy token`. See
//! "Agent tokens" in docs/design.md.

use crate::{
    access::{Grant, Level, Unlocked},
    accounts::Secret,
    time,
};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

pub mod cli;

/// Migration 26. A revoked token keeps its row, so its name stays taken
/// and the ledger names one token by it, until the sweep deletes it
/// [`KEEP_REVOKED_MS`] later. Only a full token can be unlocked.
pub const MIGRATION: &str = "CREATE TABLE agent_tokens(id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, hash BLOB NOT NULL UNIQUE, level TEXT NOT NULL CHECK(level IN ('read','triage','full')), unlocked_at INTEGER, unlocked_by TEXT, created_at INTEGER NOT NULL, used_at INTEGER, revoked_at INTEGER, CHECK(unlocked_at IS NULL OR level='full'));";

/// Undoes [`MIGRATION`], for tests that run the migrations again.
pub const UNDO: &str =
    "DROP TABLE agent_tokens; DELETE FROM sqlite_sequence WHERE name='agent_tokens';";

/// Every secret starts with this, so a secret scanner and a person can
/// tell an agent token from the admin token.
pub const PREFIX: &str = "davy_agent_";
/// Tokens not revoked, at most.
pub const MAX_LIVE: i64 = 50;
/// `used_at` is written at most this often per token.
pub const TOUCH_MS: i64 = 60_000;
/// A revoked token's row is deleted this long after it was revoked.
pub const KEEP_REVOKED_MS: i64 = 180 * 86_400_000;
const MAX_NAME: usize = 64;

/// A token's name: 1 to 64 of `a-z`, `0-9`, `.`, `_` and `-`, starting
/// with a letter or a digit, so it reads the same in a shell, a log line
/// and the web UI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Name(String);

impl Name {
    pub fn parse(s: &str) -> Result<Self, String> {
        let allowed = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || ".-_".contains(c);
        let first = s.chars().next().is_some_and(|c| c.is_ascii_alphanumeric());
        if s.len() > MAX_NAME || !first || !s.chars().all(allowed) {
            return Err(format!(
                "a token name is 1 to {MAX_NAME} of a-z, 0-9, '.', '_' and '-', starting with a letter or a digit"
            ));
        }
        Ok(Self(s.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A token as `davy token list` and the web UI show it. Never its secret
/// or hash.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(rename = "AgentToken"))]
pub struct Listed {
    pub name: String,
    pub level: Level,
    /// Whether a person unlocked destructive changes for it.
    pub destructive: bool,
    /// Who unlocked them: a person's e-mail, or `davy CLI`.
    pub unlocked_by: Option<String>,
    pub unlocked_at: Option<String>,
    pub created_at: String,
    /// The last call, to the minute, or null before the first.
    pub used_at: Option<String>,
    pub revoked_at: Option<String>,
}

/// A live token a secret named.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub id: i64,
    pub name: String,
    pub grant: Grant,
    /// `used_at` is older than [`TOUCH_MS`] and should be written.
    pub stale: bool,
}

/// A change to a token. The writer applies it for the web UI, and
/// `davy token` from its own process. `by` is who asked: a person's
/// e-mail, or `davy CLI`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    Used { id: i64, at: i64 },
    Level { name: String, level: Level },
    Unlock { name: String, by: String, at: i64 },
    Lock { name: String },
    Revoke { name: String, at: i64 },
}

/// The hash a secret is found by, as the admin token is compared.
pub fn hash(secret: &str) -> [u8; 32] {
    *blake3::hash(secret.as_bytes()).as_bytes()
}

/// Adds a token and returns it with its secret, which is not kept. The
/// inner error is a refusal: a taken name, or [`MAX_LIVE`] live tokens.
pub fn create(
    c: &Connection,
    name: &Name,
    level: Level,
    now: i64,
) -> Result<Result<(Listed, String), String>> {
    let live: i64 = c.query_row(
        "SELECT count(*) FROM agent_tokens WHERE revoked_at IS NULL",
        [],
        |r| r.get(0),
    )?;
    if live >= MAX_LIVE {
        return Ok(Err(format!(
            "there are {MAX_LIVE} tokens already: revoke one with davy token revoke"
        )));
    }
    let taken: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM agent_tokens WHERE name=?1)",
        [name.as_str()],
        |r| r.get(0),
    )?;
    if taken {
        return Ok(Err(format!(
            "a token named {:?} exists or was revoked less than 180 days ago: pick another name",
            name.as_str()
        )));
    }
    let secret = format!("{PREFIX}{}", Secret::random()?.encode());
    c.execute(
        "INSERT INTO agent_tokens(name, hash, level, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![name.as_str(), hash(&secret), level.as_str(), now],
    )?;
    let listed = one(c, name.as_str())?.ok_or_else(|| anyhow::anyhow!("token vanished"))?;
    Ok(Ok((listed, secret)))
}

/// The live token whose hash is `hash`, at `now`.
pub fn find(c: &Connection, hash: &[u8; 32], now: i64) -> Result<Option<Found>> {
    let row = c
        .query_row(
            "SELECT id, name, level, unlocked_at IS NOT NULL, used_at FROM agent_tokens
             WHERE hash=?1 AND revoked_at IS NULL",
            [hash],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, bool>(3)?,
                    r.get::<_, Option<i64>>(4)?,
                ))
            },
        )
        .optional()?;
    let Some((id, name, level, destructive, used_at)) = row else {
        return Ok(None);
    };
    let level = Level::parse(&level).map_err(anyhow::Error::msg)?;
    Ok(Some(Found {
        id,
        name,
        grant: Grant { level, destructive },
        stale: used_at.is_none_or(|at| now - at >= TOUCH_MS),
    }))
}

/// Whether the token `proof` names is still live and unlocked. The writer
/// asks it before a destructive change, in the change's transaction.
pub fn still_unlocked(c: &Connection, proof: Unlocked) -> Result<bool> {
    Ok(c.query_row(
        "SELECT EXISTS(SELECT 1 FROM agent_tokens WHERE id=?1 AND revoked_at IS NULL AND unlocked_at IS NOT NULL)",
        [proof.token()],
        |r| r.get(0),
    )?)
}

/// The refusal of a destructive change whose token was locked or revoked
/// after the call was admitted.
pub const NO_LONGER_UNLOCKED: &str = "the token was locked or revoked while the call waited for the writer, so the change was not made: a person unlocks destructive changes again on the Tokens page of the web UI or with davy token unlock";

const COLUMNS: &str =
    "name, level, unlocked_at, unlocked_by, created_at, used_at, revoked_at FROM agent_tokens";

fn listed(r: &rusqlite::Row<'_>) -> rusqlite::Result<(Listed, String)> {
    let at = |i: usize| -> rusqlite::Result<Option<String>> {
        Ok(r.get::<_, Option<i64>>(i)?.map(time::rfc3339_ms))
    };
    let unlocked_at = at(2)?;
    Ok((
        Listed {
            name: r.get(0)?,
            level: Level::Read,
            destructive: unlocked_at.is_some(),
            unlocked_by: r.get(3)?,
            unlocked_at,
            created_at: time::rfc3339_ms(r.get(4)?),
            used_at: at(5)?,
            revoked_at: at(6)?,
        },
        r.get(1)?,
    ))
}

fn with_level((mut l, level): (Listed, String)) -> Result<Listed> {
    l.level = Level::parse(&level).map_err(anyhow::Error::msg)?;
    Ok(l)
}

/// Every token, live ones first, each group newest first.
pub fn list(c: &Connection) -> Result<Vec<Listed>> {
    let mut q = c.prepare(&format!(
        "SELECT {COLUMNS} ORDER BY revoked_at IS NOT NULL, created_at DESC, id DESC"
    ))?;
    let rows = q.query_map([], listed)?;
    rows.map(|r| with_level(r?)).collect()
}

/// The token named `name`, revoked or not.
pub fn one(c: &Connection, name: &str) -> Result<Option<Listed>> {
    let row = c
        .query_row(&format!("SELECT {COLUMNS} WHERE name=?1"), [name], listed)
        .optional()?;
    row.map(with_level).transpose()
}

/// Applies `change`. The inner error names a token that does not exist
/// or was revoked, or a token not of level full to unlock.
pub fn apply(c: &Connection, change: &Change) -> Result<Result<(), String>> {
    let (sql, name, args): (&str, &str, Vec<rusqlite::types::Value>) = match change {
        Change::Used { id, at } => {
            c.execute(
                "UPDATE agent_tokens SET used_at=?2 WHERE id=?1 AND revoked_at IS NULL",
                params![id, at],
            )?;
            return Ok(Ok(()));
        }
        // A token below full keeps no unlock, so lowering one locks it.
        Change::Level { name, level } => (
            "UPDATE agent_tokens SET level=?2,
             unlocked_at=CASE WHEN ?2='full' THEN unlocked_at END,
             unlocked_by=CASE WHEN ?2='full' THEN unlocked_by END
             WHERE name=?1 AND revoked_at IS NULL",
            name,
            vec![level.as_str().to_owned().into()],
        ),
        Change::Unlock { name, by, at } => {
            if let Some(refused) = not_full(c, name)? {
                return Ok(Err(refused));
            }
            (
                "UPDATE agent_tokens SET unlocked_at=coalesce(unlocked_at, ?3), unlocked_by=coalesce(unlocked_by, ?2)
                 WHERE name=?1 AND revoked_at IS NULL",
                name,
                vec![by.clone().into(), (*at).into()],
            )
        }
        Change::Lock { name } => (
            "UPDATE agent_tokens SET unlocked_at=NULL, unlocked_by=NULL WHERE name=?1 AND revoked_at IS NULL",
            name,
            vec![],
        ),
        Change::Revoke { name, at } => (
            "UPDATE agent_tokens SET revoked_at=?2, unlocked_at=NULL, unlocked_by=NULL
             WHERE name=?1 AND revoked_at IS NULL",
            name,
            vec![(*at).into()],
        ),
    };
    let mut all: Vec<rusqlite::types::Value> = vec![name.to_owned().into()];
    all.extend(args);
    let changed = c.execute(sql, rusqlite::params_from_iter(all))?;
    Ok(if changed == 1 {
        Ok(())
    } else {
        Err(unknown(name))
    })
}

/// Why `name` cannot be unlocked, when it cannot.
fn not_full(c: &Connection, name: &str) -> Result<Option<String>> {
    Ok(match one(c, name)? {
        Some(t) if t.revoked_at.is_none() && t.level != Level::Full => Some(format!(
            "token {name:?} has level {}: only a token of level full can be unlocked for destructive changes",
            t.level.as_str()
        )),
        _ => None,
    })
}

/// The refusal for a name no live token has.
pub fn unknown(name: &str) -> String {
    format!("no token named {name:?}, or it was revoked")
}

/// Deletes revoked tokens [`KEEP_REVOKED_MS`] after their revocation.
pub fn prune(c: &Connection, at: i64) -> Result<()> {
    c.execute(
        "DELETE FROM agent_tokens WHERE revoked_at < ?1",
        [at - KEEP_REVOKED_MS],
    )?;
    Ok(())
}

#[cfg(test)]
#[path = "tokens_tests.rs"]
mod tests;
