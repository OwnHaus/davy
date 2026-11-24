//! Release health: how many sessions of each release ended in a crash.
//!
//! Browser and mobile SDKs send a `session` item when a session starts and
//! again when it changes, such as at its first error or a crash. Server SDKs
//! count requests instead and send a `sessions` item with counts per minute.
//! Davy adds both up per project, hour, release and environment, so an agent
//! can tell whether a new release crashes more than the one before.

use crate::{
    event::{MAX_ENVIRONMENT, MAX_RELEASE, Timestamp},
    items::client_report::Discarded,
    text,
};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;

const HOUR_MS: i64 = 3_600_000;
const DAY_MS: i64 = 86_400_000;
/// Sessions that started longer ago are refused. Mobile SDKs keep sessions
/// while offline, and Sentry takes them for 5 days too.
const MAX_AGE_MS: i64 = 5 * DAY_MS;
/// Clocks on phones run ahead.
const MAX_AHEAD_MS: i64 = HOUR_MS;
/// Counts read from one `sessions` item. Server SDKs flush every minute
/// with one count per minute.
const MAX_AGGREGATES: usize = 100;
/// The most one count adds.
const MAX_COUNT: u64 = 1_000_000;
/// Release and environment pairs kept per project and hour. The DSN of a
/// browser app is public, so anyone can send sessions of made-up releases.
const MAX_PAIRS: i64 = 100;
/// Sessions counted per project and UTC day. Each new session of a `session`
/// item keeps a row in `session_seen` until it leaves the window.
pub const MAX_PER_DAY: i64 = 1_000_000;
/// Days the counts are kept.
pub const KEEP_DAYS: i64 = 90;
/// The client report reasons for sessions Davy refused.
pub const OVER_DAILY_CAP: &str = "davy_daily_cap";
pub const OUT_OF_WINDOW: &str = "davy_out_of_window";

const STARTED: i64 = 1;
const ERRORED: i64 = 2;
const CRASHED: i64 = 4;
const ABNORMAL: i64 = 8;

/// One update of one session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub sid: [u8; 16],
    pub started: Timestamp,
    /// [`ERRORED`], [`CRASHED`] and [`ABNORMAL`] as of this update.
    pub flags: i64,
    pub user: Option<String>,
    pub release: String,
    pub environment: String,
}

/// The sessions of one release and environment, counted by the SDK.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Aggregates {
    pub release: String,
    pub environment: String,
    pub counts: Vec<Count>,
}

/// Sessions that started in one minute. `errored` holds the crashed ones too.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Count {
    pub started_at: Timestamp,
    pub user: Option<String>,
    pub started: i64,
    pub errored: i64,
    pub crashed: i64,
    pub abnormal: i64,
}

#[derive(Deserialize)]
struct Attrs {
    release: String,
    environment: Option<String>,
}

#[derive(Deserialize)]
struct RawSession {
    sid: String,
    did: Option<String>,
    started: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    errors: u64,
    attrs: Attrs,
}

#[derive(Deserialize)]
struct RawAggregates {
    aggregates: Vec<RawCount>,
    attrs: Attrs,
}

#[derive(Deserialize)]
struct RawCount {
    started: String,
    did: Option<String>,
    #[serde(default)]
    exited: u64,
    #[serde(default)]
    errored: u64,
    #[serde(default)]
    crashed: u64,
    #[serde(default)]
    abnormal: u64,
}

/// A `session` item. `None` when it lacks a session id, a start time or a
/// release.
pub fn parse_session(payload: &[u8]) -> Option<Session> {
    let raw: RawSession = serde_json::from_slice(payload).ok()?;
    let (release, environment) = names(&raw.attrs)?;
    let mut flags = 0;
    if raw.errors > 0 || matches!(raw.status.as_str(), "errored" | "crashed") {
        flags |= ERRORED;
    }
    match raw.status.as_str() {
        "crashed" => flags |= CRASHED,
        "abnormal" => flags |= ABNORMAL,
        _ => {}
    }
    Some(Session {
        sid: *uuid::Uuid::parse_str(&raw.sid).ok()?.as_bytes(),
        started: start(&raw.started)?,
        flags,
        user: raw.did.as_deref().and_then(user),
        release,
        environment,
    })
}

/// A `sessions` item. `None` when it lacks a release. Counts without a
/// valid start time or without sessions are skipped.
pub fn parse_aggregates(payload: &[u8]) -> Option<Aggregates> {
    let raw: RawAggregates = serde_json::from_slice(payload).ok()?;
    let (release, environment) = names(&raw.attrs)?;
    let counts = raw
        .aggregates
        .into_iter()
        .filter_map(|a| {
            let n = |v: u64| i64::try_from(v.min(MAX_COUNT)).unwrap_or(0);
            // Each request lands in one of these, so they add up.
            let started = n(a.exited) + n(a.errored) + n(a.crashed) + n(a.abnormal);
            if started == 0 {
                return None;
            }
            Some(Count {
                started_at: start(&a.started)?,
                user: a.did.as_deref().and_then(user),
                started,
                errored: n(a.errored) + n(a.crashed),
                crashed: n(a.crashed),
                abnormal: n(a.abnormal),
            })
        })
        .take(MAX_AGGREGATES)
        .collect();
    Some(Aggregates {
        release,
        environment,
        counts,
    })
}

fn names(attrs: &Attrs) -> Option<(String, String)> {
    let release = clean(&attrs.release, MAX_RELEASE)?;
    let environment = match &attrs.environment {
        Some(e) => clean(e, MAX_ENVIRONMENT)?,
        None => "production".to_owned(),
    };
    Some((release, environment))
}

/// `s` trimmed, or `None` when it is empty, too long or holds a control
/// character.
fn clean(s: &str, max: usize) -> Option<String> {
    let s = s.trim();
    let fine = !s.is_empty() && s.chars().count() <= max && text::printable(s, ' ') == s;
    fine.then(|| s.to_owned())
}

fn start(s: &str) -> Option<Timestamp> {
    Some(crate::time::parse(s)?.as_millisecond())
}

fn user(did: &str) -> Option<String> {
    clean(did, 256)
}

/// Counts one update of a session for `project`, received at `at`. Returns
/// the reason Davy refused it, if it did.
pub fn add_session(
    c: &Connection,
    project: i64,
    at: Timestamp,
    s: &Session,
) -> Result<Option<&'static str>> {
    // A session keeps the start its first update named. Later updates cannot
    // move it to another hour or keep its row alive past the window.
    let seen: Option<(i64, Timestamp)> = c
        .prepare_cached("SELECT flags, started FROM session_seen WHERE project_id=?1 AND sid=?2")?
        .query_row(params![project, s.sid], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    let (old, started) = seen.unwrap_or((0, s.started));
    if !in_window(started, at) {
        return Ok(Some(OUT_OF_WINDOW));
    }
    let new = (s.flags | STARTED) & !old;
    if new == 0 {
        return Ok(None);
    }
    let tally = Count {
        started_at: started,
        user: s.user.clone(),
        started: new & STARTED,
        errored: i64::from(new & ERRORED != 0),
        crashed: i64::from(new & CRASHED != 0),
        abnormal: i64::from(new & ABNORMAL != 0),
    };
    if let Some(refused) = bump(c, project, &s.release, &s.environment, &tally)? {
        return Ok(Some(refused));
    }
    c.prepare_cached(
        "INSERT INTO session_seen(project_id, sid, flags, started) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT DO UPDATE SET flags=excluded.flags",
    )?
    .execute(params![project, s.sid, old | new, started])?;
    Ok(None)
}

/// Counts the sessions of one `sessions` item. Returns how many counts Davy
/// refused, by reason.
pub fn add_aggregates(
    c: &Connection,
    project: i64,
    at: Timestamp,
    a: &Aggregates,
) -> Result<Vec<Discarded>> {
    let mut refused: Vec<Discarded> = Vec::new();
    for count in &a.counts {
        let reason = if in_window(count.started_at, at) {
            bump(c, project, &a.release, &a.environment, count)?
        } else {
            Some(OUT_OF_WINDOW)
        };
        let Some(reason) = reason else { continue };
        match refused.iter_mut().find(|d| d.reason == reason) {
            Some(d) => d.quantity += count.started,
            None => refused.push(Discarded {
                reason: reason.to_owned(),
                category: "session".to_owned(),
                quantity: count.started,
            }),
        }
    }
    Ok(refused)
}

fn in_window(started: Timestamp, at: Timestamp) -> bool {
    (at.saturating_sub(MAX_AGE_MS)..=at.saturating_add(MAX_AHEAD_MS)).contains(&started)
}

/// Adds `n` to the hour `n.started_at` falls in, and marks its user.
fn bump(
    c: &Connection,
    project: i64,
    release: &str,
    environment: &str,
    n: &Count,
) -> Result<Option<&'static str>> {
    let hour = n.started_at.div_euclid(HOUR_MS) * HOUR_MS;
    let day = n.started_at.div_euclid(DAY_MS) * DAY_MS;
    if n.started > 0 {
        let today: i64 = c
            .prepare_cached(
                "SELECT coalesce(sum(started), 0) FROM release_sessions
                 WHERE project_id=?1 AND hour>=?2 AND hour<?3",
            )?
            .query_row(params![project, day, day + DAY_MS], |r| r.get(0))?;
        if today.saturating_add(n.started) > MAX_PER_DAY {
            return Ok(Some(OVER_DAILY_CAP));
        }
    }
    let added = c
        .prepare_cached(
            "INSERT INTO release_sessions(project_id, hour, release, environment, started, errored, crashed, abnormal)
             SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8
             WHERE EXISTS (SELECT 1 FROM release_sessions WHERE project_id=?1 AND hour=?2 AND release=?3 AND environment=?4)
                OR (SELECT count(*) FROM release_sessions WHERE project_id=?1 AND hour=?2) < ?9
             ON CONFLICT DO UPDATE SET started=started+excluded.started, errored=errored+excluded.errored,
                crashed=crashed+excluded.crashed, abnormal=abnormal+excluded.abnormal",
        )?
        .execute(params![
            project,
            hour,
            release,
            environment,
            n.started,
            n.errored,
            n.crashed,
            n.abnormal,
            MAX_PAIRS
        ])?;
    if added == 0 {
        return Ok(Some(OVER_DAILY_CAP));
    }
    if let Some(user) = &n.user {
        let flags = (i64::from(n.errored > 0) * ERRORED) | (i64::from(n.crashed > 0) * CRASHED);
        c.prepare_cached(
            "INSERT INTO release_users(day, project_id, release, environment, user, flags)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT DO UPDATE SET flags=flags|excluded.flags",
        )?
        .execute(params![
            day,
            project,
            release,
            environment,
            user_key(project, user),
            flags
        ])?;
    }
    Ok(None)
}

/// The first 8 bytes of a hash of the project and the user's id. Davy needs
/// to count users, not to know them.
fn user_key(project: i64, did: &str) -> i64 {
    let mut h = blake3::Hasher::new();
    h.update(&project.to_le_bytes());
    h.update(did.as_bytes());
    let bytes: [u8; 8] = h.finalize().as_bytes()[..8]
        .try_into()
        .expect("a blake3 hash has 32 bytes");
    i64::from_le_bytes(bytes)
}

/// Forgets sessions that started too long ago to be counted again and
/// deletes counts older than [`KEEP_DAYS`].
pub fn prune(c: &Connection, at: Timestamp) -> Result<()> {
    c.execute(
        "DELETE FROM session_seen WHERE started<?1",
        [at - MAX_AGE_MS],
    )?;
    let cutoff = at.div_euclid(DAY_MS).saturating_sub(KEEP_DAYS) * DAY_MS;
    c.execute("DELETE FROM release_sessions WHERE hour<?1", [cutoff])?;
    c.execute("DELETE FROM release_users WHERE day<?1", [cutoff])?;
    Ok(())
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
