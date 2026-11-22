//! Rows of `users`, `sessions` and `invites`. The CLI changes users from its
//! own process; the server changes sessions and passwords through the writer
//! with a [`Change`].

use super::{
    Email, INVITE_MS, Person, SESSION_IDLE_MS, SESSION_MAX_MS, SecretHash, TOUCH_EVERY_MS,
};
use crate::{event::Timestamp, store::Outcome};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

#[derive(Serialize, Debug, PartialEq, Eq)]
pub struct User {
    pub id: i64,
    pub email: String,
    /// False until the person sets a password with a welcome link.
    pub active: bool,
    pub created_at: Timestamp,
}

/// Adds a user without a password and an invite with the hash `invite`.
/// Returns the new user's id, or `None` when the e-mail has a user.
pub fn add(
    c: &Connection,
    email: &Email,
    invite: &SecretHash,
    now: Timestamp,
) -> Result<Option<i64>> {
    let tx = c.unchecked_transaction()?;
    let inserted = tx.execute(
        "INSERT INTO users(email,created_at) VALUES (?1,?2) ON CONFLICT(email) DO NOTHING",
        params![email.as_str(), now],
    )?;
    if inserted == 0 {
        return Ok(None);
    }
    let id = tx.last_insert_rowid();
    invite_user(&tx, id, invite, now)?;
    tx.commit()?;
    Ok(Some(id))
}

/// Takes away the user's password, sessions and invites and adds an invite
/// with the hash `invite`. False when the e-mail has no user.
pub fn reset(c: &Connection, email: &Email, invite: &SecretHash, now: Timestamp) -> Result<bool> {
    let tx = c.unchecked_transaction()?;
    let Some(id) = user_id(&tx, email)? else {
        return Ok(false);
    };
    tx.execute("UPDATE users SET password=NULL WHERE id=?1", params![id])?;
    tx.execute("DELETE FROM sessions WHERE user_id=?1", params![id])?;
    tx.execute("DELETE FROM invites WHERE user_id=?1", params![id])?;
    invite_user(&tx, id, invite, now)?;
    tx.commit()?;
    Ok(true)
}

fn invite_user(c: &Connection, user: i64, invite: &SecretHash, now: Timestamp) -> Result<()> {
    c.execute(
        "INSERT INTO invites(hash,user_id,expires_at) VALUES (?1,?2,?3)",
        params![invite.as_slice(), user, now + INVITE_MS],
    )?;
    Ok(())
}

/// Removes a user with their sessions and invites. False when there is none.
pub fn remove(c: &Connection, email: &Email) -> Result<bool> {
    Ok(c.execute("DELETE FROM users WHERE email=?1", params![email.as_str()])? == 1)
}

pub fn list(c: &Connection) -> Result<Vec<User>> {
    let mut stmt =
        c.prepare("SELECT id,email,password IS NOT NULL,created_at FROM users ORDER BY email")?;
    let rows = stmt.query_map([], |r| {
        Ok(User {
            id: r.get(0)?,
            email: r.get(1)?,
            active: r.get(2)?,
            created_at: r.get(3)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn user_id(c: &Connection, email: &Email) -> Result<Option<i64>> {
    Ok(c.query_row(
        "SELECT id FROM users WHERE email=?1",
        params![email.as_str()],
        |r| r.get(0),
    )
    .optional()?)
}

/// The id and password hash of the user with `email`. The hash is `None`
/// until the person sets one.
pub fn password(c: &Connection, email: &Email) -> Result<Option<(i64, Option<String>)>> {
    Ok(c.query_row(
        "SELECT id,password FROM users WHERE email=?1",
        params![email.as_str()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()?)
}

/// The password hash of user `id`.
pub fn password_of(c: &Connection, id: i64) -> Result<Option<String>> {
    Ok(
        c.query_row("SELECT password FROM users WHERE id=?1", params![id], |r| {
            r.get(0)
        })
        .optional()?
        .flatten(),
    )
}

/// Whether `invite` is the hash of a link that still works.
pub fn invite_works(c: &Connection, invite: &SecretHash, now: Timestamp) -> Result<bool> {
    Ok(c.query_row(
        "SELECT EXISTS(SELECT 1 FROM invites WHERE hash=?1 AND expires_at>?2)",
        params![invite.as_slice(), now],
        |r| r.get(0),
    )?)
}

/// A live session: the person and whether its last use should be written
/// down again.
#[derive(Debug, PartialEq, Eq)]
pub struct Session {
    pub person: Person,
    pub stale: bool,
}

/// The session with the hash `session`, unless it ended at `now`.
pub fn session(c: &Connection, session: &SecretHash, now: Timestamp) -> Result<Option<Session>> {
    let row: Option<(i64, String, Timestamp)> = c
        .query_row(
            "SELECT u.id,u.email,s.used_at FROM sessions s JOIN users u ON u.id=s.user_id
             WHERE s.hash=?1 AND s.used_at>?2 AND s.created_at>?3",
            params![
                session.as_slice(),
                now - SESSION_IDLE_MS,
                now - SESSION_MAX_MS
            ],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    Ok(row.map(|(id, email, used_at)| Session {
        person: Person {
            id,
            email: Email(email),
        },
        stale: now - used_at >= TOUCH_EVERY_MS,
    }))
}

/// A change to sessions or passwords, applied by the writer.
#[derive(Debug)]
pub enum Change {
    /// Starts a session for `user` if their password is still `password`,
    /// the hash the login checked, so a login cannot outlive a reset that
    /// ran while it was checked.
    Login {
        user: i64,
        password: String,
        session: SecretHash,
        at: Timestamp,
    },
    Logout {
        session: SecretHash,
    },
    /// Ends every session of `user` but `keep`.
    LogoutOthers {
        user: i64,
        keep: SecretHash,
    },
    /// Uses up the invite, sets the password and starts a session.
    Welcome {
        invite: SecretHash,
        password: String,
        session: SecretHash,
        at: Timestamp,
    },
    /// Replaces the password `old` with `new` and ends every other session.
    SetPassword {
        user: i64,
        old: String,
        new: String,
        keep: SecretHash,
    },
    /// Writes down that a session was used.
    Touch {
        session: SecretHash,
        at: Timestamp,
    },
}

pub const PASSWORD_CHANGED: &str = "the password changed";
pub const LINK_USED: &str = "the link was used or expired";

pub fn apply(c: &Connection, change: &Change) -> Result<Outcome> {
    let refused = |reason: &str, changed: usize| {
        Ok(if changed == 0 {
            Err(reason.to_owned())
        } else {
            Ok(())
        })
    };
    match change {
        Change::Login {
            user,
            password,
            session,
            at,
        } => refused(
            PASSWORD_CHANGED,
            c.execute(
                "INSERT INTO sessions(hash,user_id,created_at,used_at)
                 SELECT ?1,id,?3,?3 FROM users WHERE id=?2 AND password=?4",
                params![session.as_slice(), user, at, password],
            )?,
        ),
        Change::Logout { session } => {
            c.execute(
                "DELETE FROM sessions WHERE hash=?1",
                params![session.as_slice()],
            )?;
            Ok(Ok(()))
        }
        Change::LogoutOthers { user, keep } => {
            end_others(c, *user, keep)?;
            Ok(Ok(()))
        }
        Change::Welcome {
            invite,
            password,
            session,
            at,
        } => welcome(c, invite, password, session, *at),
        Change::SetPassword {
            user,
            old,
            new,
            keep,
        } => {
            let changed = c.execute(
                "UPDATE users SET password=?3 WHERE id=?1 AND password=?2",
                params![user, old, new],
            )?;
            if changed == 1 {
                end_others(c, *user, keep)?;
            }
            refused(PASSWORD_CHANGED, changed)
        }
        Change::Touch { session, at } => {
            c.execute(
                "UPDATE sessions SET used_at=?2 WHERE hash=?1",
                params![session.as_slice(), at],
            )?;
            Ok(Ok(()))
        }
    }
}

fn end_others(c: &Connection, user: i64, keep: &SecretHash) -> Result<()> {
    c.execute(
        "DELETE FROM sessions WHERE user_id=?1 AND hash<>?2",
        params![user, keep.as_slice()],
    )?;
    Ok(())
}

fn welcome(
    c: &Connection,
    invite: &SecretHash,
    password: &str,
    session: &SecretHash,
    at: Timestamp,
) -> Result<Outcome> {
    let user: Option<i64> = c
        .query_row(
            "DELETE FROM invites WHERE hash=?1 AND expires_at>?2 RETURNING user_id",
            params![invite.as_slice(), at],
            |r| r.get(0),
        )
        .optional()?;
    let Some(user) = user else {
        return Ok(Err(LINK_USED.to_owned()));
    };
    c.execute("DELETE FROM invites WHERE user_id=?1", params![user])?;
    c.execute(
        "UPDATE users SET password=?2 WHERE id=?1",
        params![user, password],
    )?;
    c.execute(
        "INSERT INTO sessions(hash,user_id,created_at,used_at) VALUES (?1,?2,?3,?3)",
        params![session.as_slice(), user, at],
    )?;
    Ok(Ok(()))
}

/// Deletes sessions and invites that ended before `at`.
pub fn sweep(c: &Connection, at: Timestamp) -> Result<()> {
    c.execute(
        "DELETE FROM sessions WHERE used_at<=?1 OR created_at<=?2",
        params![at - SESSION_IDLE_MS, at - SESSION_MAX_MS],
    )?;
    c.execute("DELETE FROM invites WHERE expires_at<=?1", params![at])?;
    Ok(())
}
