//! How long each project keeps its logs and errors, and deleting what is
//! older in bounded steps.
//!
//! A project's own setting wins, and a project without one keeps what the
//! server default says: `davy serve --log-retention-days` and
//! `--error-retention-days`. The errors are swept by [`errors::sweep`] on
//! the writer thread, the logs by the log writer, see `src/logs/retention.rs`.

pub mod errors;

use anyhow::Result;
use rmcp::schemars::JsonSchema;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs::File,
    path::Path,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::{Duration, Instant},
};

/// 100 years. A longer window would overflow the nanosecond cutoff of logs.
pub const MAX_DAYS: u32 = 36_500;
/// Logs a project keeps when neither it nor the server says otherwise.
pub const DEFAULT_LOG_DAYS: u32 = 30;
/// Errors a project keeps when neither it nor the server says otherwise:
/// the most there is, so agents can ask about any error ever seen.
pub const DEFAULT_ERROR_DAYS: u32 = MAX_DAYS;
pub const DAY_MS: i64 = 86_400_000;

/// How long the log writer waits for the lock on the data directory before
/// it gives up the pass and tries again later. It writes logs on the same
/// thread, so a CLI that stopped while it held the lock must not stop them.
pub const SERVER_WAIT: Duration = Duration::from_secs(2);
/// How long `davy project retention` waits for that lock: a deletion under
/// way takes about one day's delete and its commit.
pub const CLI_WAIT: Duration = Duration::from_secs(300);
/// How often a wait for the lock on the data directory tries it again.
const POLL: Duration = Duration::from_millis(10);

/// Keeps a change of the retention settings apart from a deletion that
/// depends on them. The server's writer holds it while it saves a change,
/// the log writer while it reads the settings and deletes. The CLI saves a
/// change from a process of its own, so the log writer also locks the data
/// directory, which the CLI locks too.
#[derive(Clone, Debug, Default)]
pub struct Lock {
    held: Arc<Mutex<()>>,
    dir: Option<(Arc<Path>, Duration)>,
}

/// The lock of both the server and the data directory, until dropped.
pub struct Held<'a> {
    _server: MutexGuard<'a, ()>,
    _dir: Option<File>,
}

impl Lock {
    /// This lock, which [`Lock::hold_all`] also takes on the data
    /// directory `data`, waiting at most [`SERVER_WAIT`] for another
    /// process.
    #[must_use]
    pub fn with_dir(mut self, data: &Path) -> Self {
        self.dir = Some((data.into(), SERVER_WAIT));
        self
    }

    /// This lock with the data directory [`Lock::with_dir`] gave, waiting
    /// at most `wait` for it instead.
    #[must_use]
    pub fn waiting(mut self, wait: Duration) -> Self {
        if let Some((_, waits)) = &mut self.dir {
            *waits = wait;
        }
        self
    }

    /// Waits for the lock within this server. A holder that panicked
    /// guarded no data, so its poison is ignored.
    pub fn hold(&self) -> MutexGuard<'_, ()> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Waits for the lock within this server, then for the lock on the data
    /// directory, which other processes take. Always in this order, and the
    /// server's writer and the CLI each take only one of them, so no two
    /// wait for each other. Fails when the data directory is still locked
    /// after the wait it was given.
    pub fn hold_all(&self) -> Result<Held<'_>> {
        let server = self.hold();
        let dir = match &self.dir {
            None => None,
            Some((path, wait)) => Some(lock_dir(path, *wait)?),
        };
        Ok(Held {
            _server: server,
            _dir: dir,
        })
    }

    /// Whether someone holds the lock within this server now.
    #[cfg(test)]
    pub fn is_held(&self) -> bool {
        matches!(
            self.held.try_lock(),
            Err(std::sync::TryLockError::WouldBlock)
        )
    }
}

/// The directory `data` opened and locked, waiting at most `wait` for
/// another process that holds it. A directory, not a file in it: the server owns its data
/// directory and can always open it, while a lock file a CLI run as root
/// created could be one the server may not open, and a file at that path
/// could be a symlink or a FIFO. The lock goes when the directory is
/// closed, a crash included.
fn lock_dir(data: &Path, wait: Duration) -> Result<File> {
    use anyhow::Context as _;
    let dir = File::open(data).with_context(|| format!("opening {}", data.display()))?;
    anyhow::ensure!(
        dir.metadata()?.is_dir(),
        "{} is not a directory",
        data.display()
    );
    let deadline = Instant::now().checked_add(wait);
    loop {
        match dir.try_lock() {
            Ok(()) => return Ok(dir),
            Err(std::fs::TryLockError::Error(e)) => {
                return Err(e).with_context(|| format!("locking {}", data.display()));
            }
            Err(std::fs::TryLockError::WouldBlock) => {}
        }
        anyhow::ensure!(
            deadline.is_none_or(|d| Instant::now() < d),
            "{} is still locked after {} s: another Davy process holds it, such as a server deleting logs or a stopped `davy project retention`",
            data.display(),
            wait.as_secs_f64()
        );
        std::thread::sleep(POLL);
    }
}

/// Each project's own settings, NULL for the server default. id_floor
/// holds the highest id of a deleted issue, which a new issue's id exceeds.
/// issue_tags learns when each tag was last seen, so the sweep can tell
/// when the last event that carried it is gone, and gets an index by issue,
/// so deleting an issue does not scan every tag. Events get an index by
/// project and time, which is the order they expire in.
pub const MIGRATION: &str = "ALTER TABLE projects ADD COLUMN log_retention_days INTEGER CHECK(log_retention_days BETWEEN 1 AND 36500);
ALTER TABLE projects ADD COLUMN error_retention_days INTEGER CHECK(error_retention_days BETWEEN 1 AND 36500);
CREATE INDEX events_project_received ON events(project_id, received_at);
CREATE TABLE id_floor(name TEXT PRIMARY KEY, id INTEGER NOT NULL) WITHOUT ROWID;
ALTER TABLE issue_tags ADD COLUMN last_at INTEGER NOT NULL DEFAULT 0;
INSERT INTO issue_tags(key, value, issue_id, last_at) SELECT 'environment', environment, issue_id, max(received_at) FROM events WHERE environment IS NOT NULL GROUP BY environment, issue_id ON CONFLICT DO UPDATE SET last_at=excluded.last_at;
INSERT INTO issue_tags(key, value, issue_id, last_at) SELECT 'release', release, issue_id, max(received_at) FROM events WHERE release IS NOT NULL GROUP BY release, issue_id ON CONFLICT DO UPDATE SET last_at=excluded.last_at;
CREATE INDEX issue_tags_issue ON issue_tags(issue_id);
DROP TRIGGER events_tags;
CREATE TRIGGER events_tags AFTER INSERT ON events BEGIN
INSERT INTO issue_tags(key, value, issue_id, last_at) SELECT 'environment', new.environment, new.issue_id, new.received_at WHERE new.environment IS NOT NULL ON CONFLICT DO UPDATE SET last_at=max(last_at, excluded.last_at);
INSERT INTO issue_tags(key, value, issue_id, last_at) SELECT 'release', new.release, new.issue_id, new.received_at WHERE new.release IS NOT NULL ON CONFLICT DO UPDATE SET last_at=max(last_at, excluded.last_at);
END;";

/// Reverts [`MIGRATION`], for tests that run the migrations again.
#[cfg(test)]
pub const UNDO: &str = "DROP INDEX events_project_received; DROP TABLE id_floor;
ALTER TABLE projects DROP COLUMN log_retention_days;
ALTER TABLE projects DROP COLUMN error_retention_days;
DROP TRIGGER events_tags; DROP INDEX issue_tags_issue;
ALTER TABLE issue_tags DROP COLUMN last_at;
CREATE TRIGGER events_tags AFTER INSERT ON events BEGIN INSERT OR IGNORE INTO issue_tags SELECT 'environment', new.environment, new.issue_id WHERE new.environment IS NOT NULL; INSERT OR IGNORE INTO issue_tags SELECT 'release', new.release, new.issue_id WHERE new.release IS NOT NULL; END;";

/// A number of days to keep, 1 to [`MAX_DAYS`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Days(u32);

impl Days {
    pub fn new(days: u32) -> Result<Self, String> {
        if (1..=MAX_DAYS).contains(&days) {
            Ok(Self(days))
        } else {
            Err(format!("retention is 1 to {MAX_DAYS} days, not {days}"))
        }
    }

    pub fn get(self) -> u32 {
        self.0
    }

    /// The first instant, in ms, that is still kept at `at`.
    pub fn cutoff_ms(self, at: i64) -> i64 {
        at - i64::from(self.0) * DAY_MS
    }
}

/// The server's defaults, for projects without a setting of their own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Defaults {
    pub logs: Days,
    pub errors: Days,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            logs: Days(DEFAULT_LOG_DAYS),
            errors: Days(DEFAULT_ERROR_DAYS),
        }
    }
}

/// A new setting: a number of days, or `"default"` to follow the server.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(untagged)]
pub enum Setting {
    Days(u32),
    Default(ServerDefault),
}

/// The word `default`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum ServerDefault {
    Default,
}

impl Setting {
    /// The days to store, None for the server default.
    fn stored(self) -> Result<Option<Days>, String> {
        match self {
            Self::Days(n) => Days::new(n).map(Some),
            Self::Default(ServerDefault::Default) => Ok(None),
        }
    }

    /// Parses `default` or a number of days, as the CLI takes it.
    pub fn parse(s: &str) -> Result<Self, String> {
        if s == "default" {
            return Ok(Self::Default(ServerDefault::Default));
        }
        let days = s
            .parse()
            .map_err(|_not_a_number| format!("{s:?} is neither a number of days nor default"))?;
        Days::new(days).map(|d| Self::Days(d.get()))
    }
}

/// What to change for one project. A setting left out stays as it is.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(rename = "RetentionChange"))]
#[serde(deny_unknown_fields)]
pub struct Change {
    pub project: String,
    #[cfg_attr(test, ts(optional))]
    pub logs_days: Option<Setting>,
    #[cfg_attr(test, ts(optional))]
    pub errors_days: Option<Setting>,
}

impl Change {
    /// Why the change cannot be stored, if it cannot.
    pub fn check(&self) -> Result<(), String> {
        for setting in [self.logs_days, self.errors_days].into_iter().flatten() {
            setting.stored()?;
        }
        Ok(())
    }
}

/// A project's own settings as stored, None where it follows the server.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Stored {
    pub logs_days: Option<Days>,
    pub errors_days: Option<Days>,
}

/// How long one kind of data is kept.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct Kept {
    #[cfg_attr(test, ts(type = "number"))]
    pub days: Days,
    /// Whether the days come from the server, not the project.
    pub server_default: bool,
    /// RFC 3339. What is older is deleted: logs by their timestamp, errors
    /// by when Davy received them.
    pub deletes_before: String,
}

/// How long a project keeps its logs and errors.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct Retention {
    pub project: String,
    pub logs: Kept,
    pub errors: Kept,
}

impl Retention {
    fn new(project: String, stored: Stored, defaults: Defaults, now_ms: i64) -> Self {
        let kept = |own: Option<Days>, default: Days| {
            let days = own.unwrap_or(default);
            Kept {
                days,
                server_default: own.is_none(),
                deletes_before: crate::time::rfc3339_ms(days.cutoff_ms(now_ms)),
            }
        };
        Self {
            project,
            logs: kept(stored.logs_days, defaults.logs),
            errors: kept(stored.errors_days, defaults.errors),
        }
    }
}

/// A stored number of days. The schema allows only 1 to 36,500.
fn days(n: Option<u32>) -> Option<Days> {
    n.and_then(|n| Days::new(n).ok())
}

/// The stored settings of the project `slug`, or None when there is none.
pub fn stored(c: &Connection, slug: &str) -> Result<Option<Stored>> {
    Ok(c.query_row(
        "SELECT log_retention_days, error_retention_days FROM projects WHERE slug=?1",
        [slug],
        |r| {
            Ok(Stored {
                logs_days: days(r.get(0)?),
                errors_days: days(r.get(1)?),
            })
        },
    )
    .optional()?)
}

/// What every project keeps at `now_ms`, by slug.
pub fn all(c: &Connection, defaults: Defaults, now_ms: i64) -> Result<Vec<Retention>> {
    Ok(all_by_id(c, defaults, now_ms)?
        .into_iter()
        .map(|(_, k)| k)
        .collect())
}

/// [`all`] with each project's id, in one statement, so a project created
/// meanwhile is in the list with its id or not at all.
pub fn all_by_id(c: &Connection, defaults: Defaults, now_ms: i64) -> Result<Vec<(i64, Retention)>> {
    let mut q = c.prepare(
        "SELECT id, slug, log_retention_days, error_retention_days FROM projects ORDER BY slug",
    )?;
    let rows = q.query_map([], |r| {
        let stored = Stored {
            logs_days: days(r.get(2)?),
            errors_days: days(r.get(3)?),
        };
        Ok((
            r.get(0)?,
            Retention::new(r.get(1)?, stored, defaults, now_ms),
        ))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// What the project `slug` keeps at `now_ms`, or None when there is none.
pub fn one(
    c: &Connection,
    slug: &str,
    defaults: Defaults,
    now_ms: i64,
) -> Result<Option<Retention>> {
    Ok(stored(c, slug)?.map(|s| Retention::new(slug.to_owned(), s, defaults, now_ms)))
}

/// Who asks for a change. An agent may only keep data longer: a shorter
/// retention deletes data for good, and an agent's prompt can carry text
/// from someone else's error or log line. `Agent` holds the server
/// defaults, which a project on `default` keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum By {
    Person,
    Agent(Defaults),
}

/// Stores `change` and returns the project's new settings. The inner error
/// names a project that does not exist, a setting out of range, or a
/// shorter retention an agent asked for.
pub fn set(c: &Connection, change: &Change, by: By) -> Result<Result<Stored, String>> {
    if let Err(why) = change.check() {
        return Ok(Err(why));
    }
    let Some(before) = stored(c, &change.project)? else {
        return Ok(Err(unknown(&change.project)));
    };
    if let By::Agent(defaults) = by
        && let Some(why) = refused(change, before, defaults)
    {
        return Ok(Err(why));
    }
    let column = |s: Option<Setting>| -> (bool, Option<u32>) {
        let stored = s.and_then(|s| s.stored().ok());
        (s.is_some(), stored.flatten().map(Days::get))
    };
    let (logs_set, logs) = column(change.logs_days);
    let (errors_set, errors) = column(change.errors_days);
    c.execute(
        "UPDATE projects SET log_retention_days=CASE WHEN ?2 THEN ?3 ELSE log_retention_days END,
         error_retention_days=CASE WHEN ?4 THEN ?5 ELSE error_retention_days END WHERE slug=?1",
        params![change.project, logs_set, logs, errors_set, errors],
    )?;
    let Some(after) = stored(c, &change.project)? else {
        return Ok(Err(unknown(&change.project)));
    };
    if after != before {
        report(by, &change.project, after);
    }
    Ok(Ok(after))
}

/// One line in the log for each stored change, so it shows who changed
/// what.
#[expect(
    clippy::print_stderr,
    reason = "the server has no logger yet, stderr is its log"
)]
fn report(by: By, project: &str, now: Stored) {
    let shown =
        |d: Option<Days>| d.map_or("the default".to_owned(), |d| format!("{} days", d.get()));
    let who = if by == By::Person {
        "a person"
    } else {
        "an agent"
    };
    eprintln!(
        "retention: {who} set project {project:?} to logs {}, errors {}",
        shown(now.logs_days),
        shown(now.errors_days),
    );
}

/// Why an agent may not make `change`, if it may not: it would keep data
/// shorter than the project does now, or return a setting of the project's
/// own to the server default. A default that keeps data longer today can be
/// lowered tomorrow, and the project would lose the floor a person set.
fn refused(change: &Change, before: Stored, defaults: Defaults) -> Option<String> {
    let kinds = [
        ("logs", change.logs_days, before.logs_days, defaults.logs),
        (
            "errors",
            change.errors_days,
            before.errors_days,
            defaults.errors,
        ),
    ];
    kinds.into_iter().find_map(|(kind, asked, own, default)| {
        let now = own.unwrap_or(default).get();
        let asked = asked?.stored().ok()?;
        if let (None, Some(own)) = (asked, own) {
            return Some(format!(
                "an agent may not return {kind} to the server default: the project keeps them {} days of its own, and a lower default would delete what it keeps. A person does it with `davy project retention` or in the web UI",
                own.get()
            ));
        }
        let asked = asked.unwrap_or(default).get();
        (asked < now).then(|| {
            format!(
                "an agent may only keep data longer: {kind} are kept {now} days and {asked} would delete what is older for good. A person shortens it with `davy project retention` or in the web UI"
            )
        })
    })
}

/// The refusal for a project that does not exist.
pub fn unknown(slug: &str) -> String {
    format!("unknown project {slug:?}")
}

/// The projects whose logs follow a setting of their own, by project id.
pub fn log_days(c: &Connection) -> Result<HashMap<u64, u32>> {
    let mut q = c.prepare(
        "SELECT id, log_retention_days FROM projects WHERE log_retention_days IS NOT NULL",
    )?;
    let rows = q.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, u32>(1)?)))?;
    let mut days = HashMap::new();
    for row in rows {
        let (id, n) = row?;
        days.insert(id.cast_unsigned(), n);
    }
    Ok(days)
}

/// Deletes in rounds of one batch per class, until no class has a full
/// batch left or `budget` is spent. `batch(i)` deletes one batch of class
/// `i` and returns whether the batch was full, so more may wait. The first
/// round always runs, so no class gets less than one batch per call, and
/// the rounds take turns, so a class with a backlog cannot use up the
/// budget of the others.
pub fn turns(
    classes: usize,
    budget: Duration,
    mut batch: impl FnMut(usize) -> Result<bool>,
) -> Result<()> {
    let deadline = Instant::now().checked_add(budget);
    let mut more = vec![true; classes];
    loop {
        for (class, more) in more.iter_mut().enumerate() {
            if *more {
                *more = batch(class)?;
            }
        }
        if !more.contains(&true) || deadline.is_some_and(|d| Instant::now() >= d) {
            return Ok(());
        }
    }
}

#[cfg(test)]
#[path = "retention_tests.rs"]
mod tests;
