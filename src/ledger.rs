//! The ledger: what changed through Davy and who asked, newest first. Every
//! MCP call that changes something and every call the token's level refused
//! leaves a row, and so do a person's changes to issues, retention and agent
//! tokens in the web UI or with `davy token`. Reads leave none. A row keeps
//! a short summary of the arguments, never a payload or a secret. See
//! "Ledger" in docs/design.md.

pub mod describe;

use crate::{
    store::{self, Job, Write},
    time,
};
use anyhow::Result;
pub use describe::What;
use rusqlite::{Connection, OptionalExtension as _, ToSql, TransactionBehavior, params};
use serde::Serialize;

/// Migration 27. `repeats` counts the same row again within
/// [`COLLAPSE_MS`], so an agent that retries a refused call in a loop
/// leaves one row. `destructive` marks a change that deleted data, which
/// no later row pushes out, see [`add`].
pub const MIGRATION: &str = "CREATE TABLE ledger(id INTEGER PRIMARY KEY, at INTEGER NOT NULL, actor_kind TEXT NOT NULL CHECK(actor_kind IN ('admin','token','person','cli')), actor TEXT NOT NULL, client TEXT, action TEXT NOT NULL, project TEXT, issue INTEGER, target TEXT, outcome TEXT NOT NULL CHECK(outcome IN ('done','error','denied')), summary TEXT NOT NULL, repeats INTEGER NOT NULL DEFAULT 1, destructive INTEGER NOT NULL DEFAULT 0 CHECK(destructive IN (0,1)));
CREATE INDEX ledger_actor ON ledger(actor, id);";

/// Undoes [`MIGRATION`], for tests that run the migrations again. It
/// undoes [`CAPS`] with the table.
pub const UNDO: &str = "DROP TABLE ledger;";

/// Migration 28: what keeps the ledger's upkeep small whatever its size.
/// `seq` numbers each agent actor's rows of one group of [`add`]'s caps,
/// 1, 2, 3 and on in the order they came, and is null on every other row;
/// the `UPDATE` numbers the rows already there. The partial indexes hold
/// those rows by actor and number, so a cap is a range of one index.
/// `ledger_at` finds the rows [`prune`] ages out, and `ledger_destructive`
/// the changes that deleted data, which the writer counts per unlock, see
/// [`crate::tokens::refusal`]. A query uses a partial index only when its
/// `WHERE` repeats the index's terms, so [`DONE`], [`REFUSED_ROWS`] and
/// that count repeat them word for word.
pub const CAPS: &str = "ALTER TABLE ledger ADD COLUMN seq INTEGER;
UPDATE ledger SET seq=n.seq FROM (SELECT id, row_number() OVER (PARTITION BY actor, outcome='done' ORDER BY id) AS seq FROM ledger WHERE actor_kind IN ('admin','token') AND NOT (outcome='done' AND destructive=1)) AS n WHERE ledger.id=n.id;
CREATE INDEX ledger_at ON ledger(at);
CREATE INDEX ledger_done ON ledger(actor, seq) WHERE actor_kind IN ('admin','token') AND outcome='done' AND destructive=0;
CREATE INDEX ledger_refused ON ledger(actor, seq) WHERE actor_kind IN ('admin','token') AND outcome<>'done';
CREATE INDEX ledger_destructive ON ledger(actor, at) WHERE outcome='done' AND destructive=1;";

/// Rows older than this are deleted.
pub const KEEP_MS: i64 = 180 * 86_400_000;
/// The newest changes an agent made that deleted nothing, kept per actor,
/// so an agent in a loop cannot fill the disk.
pub const KEEP_DONE: i64 = 5_000;
/// The newest refused or failed calls kept per agent actor: a loop of
/// refusals says the same thing over and over.
pub const KEEP_REFUSED: i64 = 500;
/// The oldest rows [`prune`] deletes in one sweep, at most, so a backlog,
/// such as after the server was down for days, holds up the writer for
/// one batch per sweep.
pub const PRUNE_BATCH: i64 = 5_000;
/// The same row again within this long counts on the newest one.
pub const COLLAPSE_MS: i64 = 60_000;
/// The longest summary, in characters.
pub const MAX_SUMMARY: usize = 200;
/// The longest target, client name or actor, in characters.
pub const MAX_SHORT: usize = 100;
/// Rows one page holds, at most.
pub const MAX_PAGE: usize = 200;

/// The actor of the admin token. A space is in no token name and no
/// e-mail, so no other actor has this name.
pub const ADMIN: &str = "admin token";
/// The actor of a change made with `davy` on the host.
pub const CLI: &str = "davy CLI";
/// The note of an agent's change the writer refused.
pub const REFUSED: &str = "refused";
/// The note of an agent's call that ended in a tool error.
pub const TOOL_ERROR: &str = "tool error";

/// Who asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Who {
    /// The admin token.
    Admin,
    /// An agent token, by name.
    Token(String),
    /// A person signed in to the web UI, by e-mail.
    Person(String),
    /// Whoever ran `davy` on the host.
    Cli,
}

impl Who {
    fn parts(&self) -> (ActorKind, &str) {
        match self {
            Self::Admin => (ActorKind::Admin, ADMIN),
            Self::Token(name) => (ActorKind::Token, name),
            Self::Person(email) => (ActorKind::Person, email),
            Self::Cli => (ActorKind::Cli, CLI),
        }
    }
}

/// What kind of actor a row names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum ActorKind {
    Admin,
    Token,
    Person,
    Cli,
}

impl ActorKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Token => "token",
            Self::Person => "person",
            Self::Cli => "cli",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "admin" => Self::Admin,
            "token" => Self::Token,
            "person" => Self::Person,
            _ => Self::Cli,
        }
    }
}

/// How a change ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Done,
    /// The tool or the store refused it, or it failed.
    Error,
    /// The token's level does not reach it.
    Denied,
}

impl Outcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Error => "error",
            Self::Denied => "denied",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "done" => Self::Done,
            "denied" => Self::Denied,
            _ => Self::Error,
        }
    }
}

/// One change to record. [`add`] cuts every text to its limit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub at: i64,
    pub who: Who,
    /// The MCP client's name, when it gave one.
    pub client: Option<String>,
    /// The MCP tool, or the web UI's or the CLI's action.
    pub action: String,
    /// The project's slug. For an issue, the writer looks it up.
    pub project: Option<String>,
    pub issue: Option<i64>,
    /// What the change was about, such as `rule 7`.
    pub target: Option<String>,
    pub outcome: Outcome,
    pub summary: String,
    /// The change deleted data, so no later row pushes this one out, see
    /// [`add`].
    pub destructive: bool,
}

impl Entry {
    /// A change `who` made now with `action`, about `what`, done unless
    /// [`Entry::ended`] says otherwise.
    pub fn of(who: Who, action: &str, what: What) -> Self {
        Self {
            at: time::now_ms(),
            who,
            client: None,
            action: action.to_owned(),
            project: what.project,
            issue: what.issue,
            target: what.target,
            outcome: Outcome::Done,
            summary: what.summary,
            destructive: false,
        }
    }

    /// The same entry marked as a change that deletes data. In a
    /// [`Job::Recorded`] it marks a call admitted as destructive, and the
    /// writer keeps the mark only when the change was made and deleted.
    #[must_use]
    pub fn destructive(mut self) -> Self {
        self.destructive = true;
        self
    }

    /// The same entry ended as `outcome`, with `note`, why it was refused
    /// or failed, after the summary.
    #[must_use]
    pub fn ended(mut self, outcome: Outcome, note: Option<&str>) -> Self {
        self.outcome = outcome;
        if let Some(note) = note {
            self.summary = format!("{}; {note}", self.summary);
        }
        self
    }

    /// The same entry ended as the writer's `result` for its change. For
    /// an agent the refusal is [`REFUSED`] alone: its text can quote what
    /// the agent sent, and the agent got it in its answer.
    #[must_use]
    pub fn of_result(self, result: &store::Outcome) -> Self {
        let agent = matches!(self.who, Who::Admin | Who::Token(_));
        match result {
            Ok(()) => self.ended(Outcome::Done, None),
            Err(_) if agent => self.ended(Outcome::Error, Some(REFUSED)),
            Err(why) => self.ended(Outcome::Error, Some(why)),
        }
    }
}

/// A row as the web UI and the `ledger` tool show it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(rename = "LedgerRow"))]
pub struct Row {
    pub id: i64,
    pub at: String,
    pub actor_kind: ActorKind,
    pub actor: String,
    pub client: Option<String>,
    pub action: String,
    pub project: Option<String>,
    pub issue: Option<i64>,
    pub target: Option<String>,
    pub outcome: Outcome,
    pub summary: String,
    /// How often the same row came within a minute of the one before.
    pub repeats: i64,
}

/// `s` cut to `max` characters, with an ellipsis when it was longer, and
/// every control character and mark that reorders text made a space, as
/// part of a summary is text an agent sent.
pub fn cut(s: &str, max: usize) -> String {
    let s = crate::text::printable(s, ' ');
    if s.chars().count() <= max {
        return s.into_owned();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Records `e`, or counts it on the newest row of the table when that row
/// is the same change by the same actor less than [`COLLAPSE_MS`] before.
/// Only the newest of all, so a count never hides that another change
/// came in between. A new row of an agent actor, the admin token or a
/// token, then pushes out the oldest of its group past the cap, see
/// [`insert`], so the writer's cost per row stays the same however many rows
/// the ledger holds.
pub fn add(c: &Connection, e: &Entry) -> Result<()> {
    let (kind, actor) = e.who.parts();
    let actor = cut(actor, MAX_SHORT);
    let client = e.client.as_deref().map(|s| cut(s, MAX_SHORT));
    let project = match (&e.project, e.issue) {
        (Some(slug), _) => Some(cut(slug, MAX_SHORT)),
        (None, Some(issue)) => c
            .query_row(
                "SELECT p.slug FROM issues i JOIN projects p ON p.id=i.project_id WHERE i.id=?1",
                [issue],
                |r| r.get(0),
            )
            .optional()?,
        (None, None) => None,
    };
    let target = e.target.as_deref().map(|s| cut(s, MAX_SHORT));
    let summary = cut(&e.summary, MAX_SUMMARY);
    let values: [&dyn ToSql; 11] = [
        &e.at,
        &kind.as_str(),
        &actor,
        &client,
        &e.action,
        &project,
        &e.issue,
        &target,
        &e.outcome.as_str(),
        &summary,
        &e.destructive,
    ];
    let same = c.execute(
        "UPDATE ledger SET repeats=repeats+1, at=?1 WHERE id=(SELECT max(id) FROM ledger)
         AND at>=?1-?12 AND actor=?3 AND actor_kind=?2 AND client IS ?4 AND action=?5 AND project IS ?6
         AND issue IS ?7 AND target IS ?8 AND outcome=?9 AND summary=?10 AND destructive=?11",
        [values.as_slice(), &[&COLLAPSE_MS]].concat().as_slice(),
    )?;
    if same == 0 {
        insert(c, &actor, e, &values)?;
    }
    Ok(())
}

/// The done changes of an agent actor that deleted nothing, and the rows
/// [`insert`] keeps of them.
const DONE: (&str, i64) = (
    "actor_kind IN ('admin','token') AND outcome='done' AND destructive=0",
    KEEP_DONE,
);
/// The refused, denied and failed calls of an agent actor, and the rows
/// [`insert`] keeps of them.
const REFUSED_ROWS: (&str, i64) = (
    "actor_kind IN ('admin','token') AND outcome<>'done'",
    KEEP_REFUSED,
);

/// Inserts the row of `e` and deletes the rows of `actor` in its group
/// past the group's cap, oldest first. Each group only pushes out rows of
/// its own, so a loop of refusals never pushes out a change. A change that
/// deleted data is in no group: nothing but age deletes it, so a token
/// cannot erase the record of its own destructive changes, and the writer
/// refuses more than [`crate::tokens::MAX_DESTRUCTIVE`] of them per unlock
/// instead. A person's rows and the CLI's, made at a person's pace, are in
/// none either. The row takes the next number of its group, see [`CAPS`],
/// so the rows past the cap are those numbered up to the cap below it:
/// the newest number and that range each take one search of one index,
/// whatever the ledger's size.
fn insert(c: &Connection, actor: &str, e: &Entry, values: &[&dyn ToSql; 11]) -> Result<()> {
    let group = match (&e.who, e.outcome) {
        (Who::Person(_) | Who::Cli, _) => None,
        (Who::Admin | Who::Token(_), Outcome::Done) if e.destructive => None,
        (Who::Admin | Who::Token(_), Outcome::Done) => Some(DONE),
        (Who::Admin | Who::Token(_), Outcome::Error | Outcome::Denied) => Some(REFUSED_ROWS),
    };
    let seq: Option<i64> = match group {
        Some((terms, _)) => Some(
            c.prepare_cached(&numbered(terms))?
                .query_row([actor], |r| r.get(0))?,
        ),
        None => None,
    };
    c.prepare_cached(
        "INSERT INTO ledger(at, actor_kind, actor, client, action, project, issue, target, outcome, summary, destructive, seq)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
    )?
    .execute([values.as_slice(), &[&seq]].concat().as_slice())?;
    if let (Some((terms, keep)), Some(seq)) = (group, seq) {
        c.prepare_cached(&capped(terms))?
            .execute(params![actor, seq - keep])?;
    }
    Ok(())
}

/// The number the next row of an actor in `group` takes.
fn numbered(group: &str) -> String {
    format!("SELECT coalesce(max(seq), 0) + 1 FROM ledger WHERE actor=?1 AND {group}")
}

/// Deletes an actor's rows in `group` numbered up to a number.
fn capped(group: &str) -> String {
    format!("DELETE FROM ledger WHERE actor=?1 AND {group} AND seq <= ?2")
}

/// Queues `entry` on `writes` and waits until the writer stored it, so
/// an agent that reads the ledger next sees its row. A full queue makes
/// the caller wait for room, off the runtime, so a busy agent slows down
/// rather than rows piling up in memory. A writer that stopped loses it.
pub async fn record(writes: &store::Sender, entry: Entry) {
    let (done, stored) = tokio::sync::oneshot::channel();
    let write = Write {
        job: Job::Ledger(entry),
        done,
    };
    let queued = match writes.try_send(write) {
        Ok(()) => true,
        Err(crossbeam_channel::TrySendError::Full(write)) => {
            let writes = writes.clone();
            let sent = tokio::task::spawn_blocking(move || writes.send(write).is_ok()).await;
            matches!(sent, Ok(true))
        }
        Err(crossbeam_channel::TrySendError::Disconnected(_)) => false,
    };
    if queued {
        drop(stored.await);
    }
}

/// Runs `change` on `c` and records `entry` as it ended, in one
/// transaction, for `davy` on the host, which writes from its own process.
/// The inner error is the change's refusal, recorded as an error.
pub fn recorded<T>(
    c: &Connection,
    entry: Entry,
    change: impl FnOnce(&Connection) -> Result<std::result::Result<T, String>>,
) -> Result<std::result::Result<T, String>> {
    recorded_as(c, |tx| Ok((change(tx)?, Some(entry))))
}

/// Like [`recorded`], where the change also gives its row, from what it
/// found in the transaction: the rule it removed, say. No row when it
/// gives none, for a change that changed nothing.
pub fn recorded_as<T>(
    c: &Connection,
    change: impl FnOnce(&Connection) -> Result<(std::result::Result<T, String>, Option<Entry>)>,
) -> Result<std::result::Result<T, String>> {
    let tx = rusqlite::Transaction::new_unchecked(c, TransactionBehavior::Immediate)?;
    let (result, entry) = change(&tx)?;
    if let Some(entry) = entry {
        let ended = result.as_ref().map(drop).map_err(Clone::clone);
        add(&tx, &entry.of_result(&ended))?;
    }
    tx.commit()?;
    Ok(result)
}

/// Up to `limit` rows older than row `before`, newest first, of `actor`
/// alone when it is given.
pub fn list(
    c: &Connection,
    actor: Option<&str>,
    before: Option<i64>,
    limit: usize,
) -> Result<Vec<Row>> {
    let mut q = c.prepare(
        "SELECT id, at, actor_kind, actor, client, action, project, issue, target, outcome, summary, repeats
         FROM ledger WHERE (?1 IS NULL OR actor=?1) AND id<?2 ORDER BY id DESC LIMIT ?3",
    )?;
    let limit = i64::try_from(limit.min(MAX_PAGE))?;
    let rows = q.query_map(params![actor, before.unwrap_or(i64::MAX), limit], |r| {
        Ok(Row {
            id: r.get(0)?,
            at: time::rfc3339_ms(r.get(1)?),
            actor_kind: ActorKind::parse(&r.get::<_, String>(2)?),
            actor: r.get(3)?,
            client: r.get(4)?,
            action: r.get(5)?,
            project: r.get(6)?,
            issue: r.get(7)?,
            target: r.get(8)?,
            outcome: Outcome::parse(&r.get::<_, String>(9)?),
            summary: r.get(10)?,
            repeats: r.get(11)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Every actor with a row, for the filter of the web UI.
pub fn actors(c: &Connection) -> Result<Vec<String>> {
    let mut q = c.prepare("SELECT DISTINCT actor FROM ledger ORDER BY actor")?;
    let rows = q.query_map([], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Deletes up to [`PRUNE_BATCH`] rows older than [`KEEP_MS`], oldest
/// first, found by `ledger_at`. [`add`] keeps the caps as rows come, so
/// the sweep reads only the rows it deletes.
pub fn prune(c: &Connection, at: i64) -> Result<()> {
    c.execute(PRUNE, params![at - KEEP_MS, PRUNE_BATCH])?;
    Ok(())
}

/// Deletes up to `?2` rows older than `?1`, oldest first.
const PRUNE: &str =
    "DELETE FROM ledger WHERE id IN (SELECT id FROM ledger WHERE at < ?1 ORDER BY at LIMIT ?2)";

#[cfg(test)]
#[path = "ledger_tests.rs"]
mod tests;
