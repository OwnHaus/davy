const SCHEMA: &str = "PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;
CREATE TABLE IF NOT EXISTS projects(id INTEGER PRIMARY KEY, slug TEXT UNIQUE NOT NULL, public_key TEXT UNIQUE NOT NULL, created_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS issues(id INTEGER PRIMARY KEY, project_id INTEGER NOT NULL REFERENCES projects(id), grouping_version INTEGER NOT NULL, fingerprint TEXT NOT NULL, title TEXT NOT NULL, culprit TEXT, level TEXT NOT NULL, status TEXT NOT NULL, resolved_in_release TEXT, resolved_at INTEGER, regressed_at INTEGER, first_seen INTEGER NOT NULL, last_seen INTEGER NOT NULL, event_count INTEGER NOT NULL, UNIQUE(project_id, grouping_version, fingerprint));
CREATE TABLE IF NOT EXISTS events(id INTEGER PRIMARY KEY, project_id INTEGER NOT NULL REFERENCES projects(id), issue_id INTEGER NOT NULL REFERENCES issues(id), event_id TEXT NOT NULL, timestamp INTEGER NOT NULL, received_at INTEGER NOT NULL, level TEXT NOT NULL, release TEXT, environment TEXT, trace_id TEXT, user_id TEXT, payload BLOB, UNIQUE(project_id, event_id));
CREATE TABLE IF NOT EXISTS issue_hourly(issue_id INTEGER NOT NULL REFERENCES issues(id), hour INTEGER NOT NULL, count INTEGER NOT NULL, PRIMARY KEY(issue_id, hour));
CREATE INDEX IF NOT EXISTS events_issue ON events(issue_id, id DESC);";
/// Schema changes after the first release, applied in order. `PRAGMA
/// user_version` counts how many ran.
const MIGRATIONS: [&str; 9] = [
    "CREATE TABLE releases(project_id INTEGER NOT NULL REFERENCES projects(id), version TEXT NOT NULL, first_seen INTEGER NOT NULL, PRIMARY KEY(project_id, version));
INSERT INTO releases SELECT project_id, release, min(received_at) FROM events WHERE release IS NOT NULL GROUP BY project_id, release;",
    "CREATE TABLE monitors(id INTEGER PRIMARY KEY, project_id INTEGER NOT NULL REFERENCES projects(id), name TEXT NOT NULL, kind TEXT NOT NULL, url TEXT, expect TEXT, keyword TEXT, heartbeat_token TEXT UNIQUE, every_s INTEGER NOT NULL, timeout_s INTEGER, grace_s INTEGER, state TEXT NOT NULL, last_check_at INTEGER, last_ping_at INTEGER, created_at INTEGER NOT NULL);
CREATE TABLE checks(monitor_id INTEGER NOT NULL REFERENCES monitors(id) ON DELETE CASCADE, at INTEGER NOT NULL, ok INTEGER NOT NULL, status INTEGER, ms INTEGER, reason TEXT);
CREATE INDEX checks_monitor_at ON checks(monitor_id, at);
CREATE TABLE incidents(id INTEGER PRIMARY KEY, monitor_id INTEGER NOT NULL REFERENCES monitors(id) ON DELETE CASCADE, started_at INTEGER NOT NULL, resolved_at INTEGER, reason TEXT NOT NULL);
CREATE INDEX incidents_open ON incidents(monitor_id) WHERE resolved_at IS NULL;",
    "CREATE TABLE channels(id INTEGER PRIMARY KEY, project_id INTEGER REFERENCES projects(id), kind TEXT NOT NULL, url TEXT NOT NULL, secret TEXT, dropped INTEGER NOT NULL DEFAULT 0, created_at INTEGER NOT NULL);
CREATE TABLE notifications(id INTEGER PRIMARY KEY, channel_id INTEGER NOT NULL REFERENCES channels(id) ON DELETE CASCADE, type TEXT NOT NULL, payload TEXT NOT NULL, created_at INTEGER NOT NULL, attempts INTEGER NOT NULL DEFAULT 0, next_at INTEGER NOT NULL, sent_at INTEGER, failed_at INTEGER, last_error TEXT);
CREATE INDEX notifications_pending ON notifications(channel_id, id) WHERE sent_at IS NULL AND failed_at IS NULL;",
    "CREATE TABLE feedback(id INTEGER PRIMARY KEY, at INTEGER NOT NULL, client TEXT, tried TEXT NOT NULL, happened TEXT NOT NULL, fix TEXT);
ALTER TABLE channels ADD COLUMN feedback INTEGER NOT NULL DEFAULT 0;",
    "CREATE TABLE users(id INTEGER PRIMARY KEY, email TEXT UNIQUE NOT NULL, password TEXT, created_at INTEGER NOT NULL);
CREATE TABLE sessions(hash BLOB PRIMARY KEY, user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE, created_at INTEGER NOT NULL, used_at INTEGER NOT NULL);
CREATE INDEX sessions_user ON sessions(user_id);
CREATE TABLE invites(hash BLOB PRIMARY KEY, user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE, expires_at INTEGER NOT NULL);
CREATE INDEX invites_user ON invites(user_id);",
    "CREATE INDEX IF NOT EXISTS issues_project_status ON issues(project_id, status, last_seen DESC);",
    // The issues list. Each index holds every sort key, so a list in any
    // order with any status reads only the index, and two indexes instead of
    // nine keep ingest, which updates last_seen and event_count, cheap.
    // issues_text and issue_tags turn the text, environment and release
    // filters into sets of ids instead of a scan. Timings in docs/design.md,
    // "Load check".
    "DROP INDEX IF EXISTS issues_last_seen;
DROP INDEX IF EXISTS issues_project_status;
CREATE INDEX issues_status_sorts ON issues(status, last_seen, event_count, first_seen);
CREATE INDEX issues_project_sorts ON issues(project_id, status, last_seen, event_count, first_seen);
CREATE VIRTUAL TABLE issues_text USING fts5(title, culprit, content='issues', content_rowid='id', tokenize='trigram');
INSERT INTO issues_text(issues_text) VALUES('rebuild');
CREATE TRIGGER issues_text_insert AFTER INSERT ON issues BEGIN INSERT INTO issues_text(rowid, title, culprit) VALUES (new.id, new.title, new.culprit); END;
CREATE TRIGGER issues_text_update AFTER UPDATE OF title, culprit ON issues BEGIN INSERT INTO issues_text(issues_text, rowid, title, culprit) VALUES ('delete', old.id, old.title, old.culprit); INSERT INTO issues_text(rowid, title, culprit) VALUES (new.id, new.title, new.culprit); END;
CREATE TRIGGER issues_text_delete AFTER DELETE ON issues BEGIN INSERT INTO issues_text(issues_text, rowid, title, culprit) VALUES ('delete', old.id, old.title, old.culprit); END;
CREATE TABLE issue_tags(key TEXT NOT NULL CHECK(key IN ('environment', 'release')), value TEXT NOT NULL, issue_id INTEGER NOT NULL REFERENCES issues(id), PRIMARY KEY(key, value, issue_id)) WITHOUT ROWID;
INSERT OR IGNORE INTO issue_tags SELECT 'environment', environment, issue_id FROM events WHERE environment IS NOT NULL;
INSERT OR IGNORE INTO issue_tags SELECT 'release', release, issue_id FROM events WHERE release IS NOT NULL;
CREATE TRIGGER events_tags AFTER INSERT ON events BEGIN INSERT OR IGNORE INTO issue_tags SELECT 'environment', new.environment, new.issue_id WHERE new.environment IS NOT NULL; INSERT OR IGNORE INTO issue_tags SELECT 'release', new.release, new.issue_id WHERE new.release IS NOT NULL; END;",
    // What the setup page shows that no other table holds.
    "CREATE TABLE firsts(what TEXT PRIMARY KEY, at INTEGER NOT NULL);",
    // Client reports and user feedback from the SDKs, see src/items.
    "CREATE TABLE client_reports(day INTEGER NOT NULL, project_id INTEGER NOT NULL REFERENCES projects(id), reason TEXT NOT NULL, category TEXT NOT NULL, quantity INTEGER NOT NULL, PRIMARY KEY(day, project_id, reason, category)) WITHOUT ROWID;
CREATE TABLE user_feedback(id INTEGER PRIMARY KEY, project_id INTEGER NOT NULL REFERENCES projects(id), received_at INTEGER NOT NULL, event_id TEXT, issue_id INTEGER REFERENCES issues(id), message TEXT NOT NULL, name TEXT, email TEXT, url TEXT);
CREATE INDEX user_feedback_project ON user_feedback(project_id, received_at);
CREATE INDEX user_feedback_issue ON user_feedback(issue_id, received_at) WHERE issue_id IS NOT NULL;",
];
mod latest;
use crate::{
    alert,
    event::{ErrorEvent, Timestamp},
    grouping::group,
    time,
};
use anyhow::Result;
use crossbeam_channel::{Receiver, TrySendError, select_biased};
use latest::Latest;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;
use std::{path::Path, thread};
use tokio::sync::oneshot;

const QUEUE_FULL_WAIT: std::time::Duration = std::time::Duration::from_millis(50);
/// The queue of jobs from people in the web UI, apart from ingest, so a full
/// ingest queue does not refuse a login.
pub const UI_QUEUE: usize = 64;
const BATCH: usize = 64;

/// Where jobs for the writer go.
pub type Sender = crossbeam_channel::Sender<Write>;

#[derive(Debug, PartialEq, Eq)]
pub enum IssueStatus {
    Unresolved,
    Resolved {
        in_release: Option<String>,
        at: Timestamp,
    },
    Ignored,
}
impl IssueStatus {
    /// Applies a new event and returns `at` when the event reopens a resolved
    /// issue. `release` is the event's release and when Davy first saw it.
    pub fn on_event(
        &mut self,
        release: Option<(&str, Timestamp)>,
        at: Timestamp,
    ) -> Option<Timestamp> {
        let Self::Resolved {
            in_release,
            at: resolved_at,
        } = self
        else {
            return None;
        };
        let reopens = release.is_none_or(|(version, first_seen)| {
            in_release.as_deref() == Some(version) || first_seen > *resolved_at
        });
        reopens.then(|| {
            *self = Self::Unresolved;
            at
        })
    }
    fn load(status: &str, in_release: Option<String>, at: Option<Timestamp>) -> Self {
        match status {
            "resolved" => Self::Resolved {
                in_release,
                at: at.unwrap_or(0),
            },
            "ignored" => Self::Ignored,
            _ => Self::Unresolved,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unresolved => "unresolved",
            Self::Resolved { .. } => "resolved",
            Self::Ignored => "ignored",
        }
    }
}
/// The database file in a data directory.
pub const DB_FILE: &str = "davy.sqlite";
/// How long a connection waits for another one's write lock.
pub const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Opens the database in `data`, and creates a missing `data` readable by
/// its owner only, because it holds events, logs and keys.
pub fn open(data: &Path) -> Result<Connection> {
    let mut dir = std::fs::DirBuilder::new();
    dir.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut dir, 0o700);
    dir.create(data)?;
    let mut c = Connection::open(data.join(DB_FILE))?;
    c.busy_timeout(BUSY_TIMEOUT)?;
    init(&mut c)?;
    Ok(c)
}
/// Creates the schema and runs pending migrations. The server and a CLI
/// process may open the same directory at once, so the version is read inside
/// a write transaction.
pub fn init(c: &mut Connection) -> Result<()> {
    c.execute_batch(SCHEMA)?;
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let done: u32 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    for (version, sql) in (1_u32..).zip(MIGRATIONS).filter(|(v, _)| *v > done) {
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", version)?;
    }
    tx.commit()?;
    Ok(())
}
/// Creates the project, or returns the one that has this slug, so running
/// `davy project create` again prints the same DSN.
pub fn create_project(c: &Connection, slug: &str) -> Result<(i64, String)> {
    let key = uuid::Uuid::new_v4().simple().to_string();
    c.execute(
        "INSERT INTO projects(slug,public_key,created_at) VALUES (?1,?2,?3) ON CONFLICT(slug) DO NOTHING",
        params![slug, key, time::now_ms()],
    )?;
    Ok(c.query_row(
        "SELECT id, public_key FROM projects WHERE slug=?1",
        params![slug],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?)
}
pub fn project_for_key(c: &Connection, key: &str) -> Result<Option<i64>> {
    Ok(c.query_row(
        "SELECT id FROM projects WHERE public_key=?1",
        params![key],
        |r| r.get(0),
    )
    .optional()?)
}
/// The id of the project `slug`, or an error that names it.
pub fn project_by_slug(c: &Connection, slug: &str) -> Result<i64> {
    project_id(c, slug)?.ok_or_else(|| anyhow::anyhow!("unknown project {slug:?}"))
}
pub fn project_id(c: &Connection, slug: &str) -> Result<Option<i64>> {
    Ok(c.query_row(
        "SELECT id FROM projects WHERE slug=?1",
        params![slug],
        |r| r.get(0),
    )
    .optional()?)
}
/// Writes one event in a transaction of its own.
pub fn insert(c: &Connection, project: i64, e: &ErrorEvent) -> Result<()> {
    let mut latest = Latest::default();
    add(c, project, e, &mut latest)?;
    latest.keep(c)
}
/// Writes one event. `latest` gives the newest event of each issue its
/// payload when the transaction ends.
fn add<'a>(c: &Connection, project: i64, e: &'a ErrorEvent, latest: &mut Latest<'a>) -> Result<()> {
    let duplicate: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM events WHERE project_id=?1 AND event_id=?2)",
        params![project, e.event_id.0],
        |r| r.get(0),
    )?;
    if duplicate {
        return Ok(());
    }
    let g = group(e);
    let created = c.execute("INSERT INTO issues(project_id,grouping_version,fingerprint,title,culprit,level,status,first_seen,last_seen,event_count) VALUES (?1,?2,?3,?4,?5,?6,'unresolved',?7,?7,0) ON CONFLICT(project_id,grouping_version,fingerprint) DO NOTHING", params![project,g.version,g.fingerprint,g.title,g.culprit,e.level.as_str(),e.timestamp])? == 1;
    let (id,count,status,release,at,title,culprit): (i64,i64,String,Option<String>,Option<i64>,String,Option<String>) = c.query_row("SELECT id,event_count,status,resolved_in_release,resolved_at,title,culprit FROM issues WHERE project_id=?1 AND grouping_version=?2 AND fingerprint=?3", params![project,g.version,g.fingerprint], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?)))?;
    let mut status = IssueStatus::load(&status, release, at);
    let seen = match &e.release {
        Some(version) => {
            c.execute(
                "INSERT INTO releases(project_id,version,first_seen) VALUES (?1,?2,?3) ON CONFLICT DO NOTHING",
                params![project, version, e.received_at],
            )?;
            let first_seen: Timestamp = c.query_row(
                "SELECT first_seen FROM releases WHERE project_id=?1 AND version=?2",
                params![project, version],
                |r| r.get(0),
            )?;
            Some((version.as_str(), first_seen))
        }
        None => None,
    };
    let regressed = status.on_event(seen, e.received_at);
    let status = status.as_str();
    let kept = latest.before(c, id, count)?;
    let payload = kept.then(|| latest::compress(e)).transpose()?;
    c.execute("INSERT INTO events(project_id,issue_id,event_id,timestamp,received_at,level,release,environment,trace_id,user_id,payload) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",params![project,id,e.event_id.0,e.timestamp,e.received_at,e.level.as_str(),e.release,e.environment,e.trace_id,e.user_id,payload])?;
    latest.after(id, c.last_insert_rowid(), e, kept);
    c.execute("UPDATE issues SET event_count=event_count+1,first_seen=min(first_seen,?2),last_seen=max(last_seen,?2),status=?3,regressed_at=coalesce(?4,regressed_at),resolved_at=CASE WHEN ?4 IS NOT NULL THEN NULL ELSE resolved_at END,resolved_in_release=CASE WHEN ?4 IS NOT NULL THEN NULL ELSE resolved_in_release END WHERE id=?1",params![id,e.timestamp,status,regressed])?;
    c.execute("INSERT INTO issue_hourly(issue_id,hour,count) VALUES (?1,?2,1) ON CONFLICT(issue_id,hour) DO UPDATE SET count=count+1", params![id,e.timestamp.div_euclid(3_600_000)*3_600_000])?;
    if created || regressed.is_some() {
        let issue = alert::Issue {
            id,
            title,
            culprit,
            level: e.level.as_str().to_owned(),
            release: e.release.clone(),
        };
        let notice = if created {
            alert::Notice::IssueNew(issue)
        } else {
            alert::Notice::IssueRegressed(issue)
        };
        alert::store::notify(c, project, &notice, e.received_at)?;
    }
    Ok(())
}
pub enum Job {
    Ingest {
        project: i64,
        events: Vec<ErrorEvent>,
        /// What the request paid for the events. It goes back to the budget
        /// when the writer is done with the job, not when the client hangs up.
        held: crate::http::body::Held,
    },
    /// A new status for one or more issues, all of them or none.
    SetStatus {
        issues: crate::issues::IssueIds,
        status: IssueStatus,
    },
    Ping {
        monitor: i64,
        at: Timestamp,
        failed: bool,
    },
    Sweep {
        at: Timestamp,
    },
    /// The result of one HTTP check that started at `at`.
    Check {
        monitor: i64,
        at: Timestamp,
        checked: crate::monitor::check::Checked,
    },
    /// The result of one attempt to send a notification.
    Delivery {
        id: i64,
        at: Timestamp,
        result: Outcome,
    },
    /// Client reports and user feedback from one envelope.
    Items {
        project: i64,
        at: Timestamp,
        items: Vec<crate::items::SdkItem>,
    },
    /// What an agent reported with `submit_feedback`.
    Feedback {
        at: Timestamp,
        feedback: crate::feedback::Feedback,
    },
    /// A login, a logout or a new password from the web UI.
    Account(crate::accounts::store::Change),
    /// The first time Davy saw `what`. A later one changes nothing.
    First {
        what: crate::firsts::First,
        at: Timestamp,
    },
}
pub type Outcome = std::result::Result<(), String>;
pub struct Write {
    pub job: Job,
    pub done: oneshot::Sender<Outcome>,
}
/// Runs one job. The outer error is a database failure that aborts the batch,
/// the inner one a problem with this job alone.
fn apply<'a>(c: &Connection, job: &'a Job, latest: &mut Latest<'a>) -> Result<Outcome> {
    match job {
        Job::Ingest {
            project, events, ..
        } => {
            for e in events {
                add(c, *project, e, latest)?;
                if e.symbolicated {
                    crate::firsts::record(c, crate::firsts::First::Symbolicated, e.received_at)?;
                }
            }
            Ok(Ok(()))
        }
        Job::SetStatus { issues, status } => crate::issues::set_status(c, issues, status),
        Job::Ping {
            monitor,
            at,
            failed,
        } => {
            let found = crate::monitor::store::ping(c, *monitor, *at, *failed)?;
            Ok(found
                .then_some(())
                .ok_or_else(|| format!("no monitor {monitor}")))
        }
        Job::Sweep { at } => {
            crate::monitor::store::sweep(c, *at)?;
            crate::accounts::store::sweep(c, *at)?;
            crate::items::client_report::prune(c, *at)?;
            Ok(Ok(()))
        }
        Job::Check {
            monitor,
            at,
            checked,
        } => {
            crate::monitor::store::checked(c, *monitor, *at, checked)?;
            Ok(Ok(()))
        }
        Job::Delivery { id, at, result } => {
            alert::store::delivered(c, *id, *at, result)?;
            Ok(Ok(()))
        }
        Job::Items { project, at, items } => {
            crate::items::add(c, *project, *at, items)?;
            Ok(Ok(()))
        }
        Job::Feedback { at, feedback } => {
            crate::feedback::add(c, feedback, *at)?;
            Ok(Ok(()))
        }
        Job::Account(change) => crate::accounts::store::apply(c, change),
        Job::First { what, at } => {
            crate::firsts::record(c, *what, *at)?;
            Ok(Ok(()))
        }
    }
}
pub struct Writer {
    /// Ingest, checks, deliveries and sweeps.
    pub sender: Sender,
    /// Jobs from people in the web UI, [`UI_QUEUE`] deep and taken first.
    pub ui: Sender,
    pub thread: thread::JoinHandle<()>,
}
impl Writer {
    pub fn start(mut c: Connection, capacity: usize) -> Self {
        let (sender, ingest) = crossbeam_channel::bounded::<Write>(capacity);
        let (ui, people) = crossbeam_channel::bounded::<Write>(UI_QUEUE);
        let thread = thread::spawn(move || {
            let mut queues = Queues::new(people, ingest);
            while let Some(batch) = queues.next_batch() {
                let outcomes = (|| -> Result<Vec<Outcome>> {
                    let tx = c.transaction()?;
                    let mut latest = Latest::default();
                    let outcomes = batch
                        .iter()
                        .map(|w| apply(&tx, &w.job, &mut latest))
                        .collect::<Result<_>>()?;
                    latest.keep(&tx)?;
                    tx.commit()?;
                    Ok(outcomes)
                })()
                .unwrap_or_else(|e| vec![Err(e.to_string()); batch.len()]);
                for (write, outcome) in batch.into_iter().zip(outcomes) {
                    #[expect(
                        clippy::let_underscore_must_use,
                        reason = "a closed receiver means the client hung up, and the batch is written either way"
                    )]
                    let _ = write.done.send(outcome);
                }
            }
        });
        Self { sender, ui, thread }
    }
}
/// The writer's two queues. A queue whose senders are all gone is replaced by
/// one that never yields, and the writer stops when both are gone.
struct Queues {
    ui: Receiver<Write>,
    ingest: Receiver<Write>,
    open: u8,
}
impl Queues {
    fn new(ui: Receiver<Write>, ingest: Receiver<Write>) -> Self {
        Self {
            ui,
            ingest,
            open: 2,
        }
    }
    /// Up to [`BATCH`] jobs, those of people first but at most half of them
    /// while ingest waits, so people who keep writing cannot stop ingest.
    /// Waits for the first one.
    fn next_batch(&mut self) -> Option<Vec<Write>> {
        let first = loop {
            if self.open == 0 {
                return None;
            }
            let got = select_biased! {
                recv(self.ui) -> w => w.map_err(|_gone| &mut self.ui),
                recv(self.ingest) -> w => w.map_err(|_gone| &mut self.ingest),
            };
            match got {
                Ok(w) => break w,
                Err(gone) => {
                    *gone = crossbeam_channel::never();
                    self.open -= 1;
                }
            }
        };
        let mut batch = vec![first];
        batch.extend(self.ui.try_iter().take(BATCH / 2 - 1));
        let room = BATCH - batch.len();
        batch.extend(self.ingest.try_iter().take(room));
        let room = BATCH - batch.len();
        batch.extend(self.ui.try_iter().take(room));
        Some(batch)
    }
}
/// Why the writer did not apply a job given to [`submit`].
#[derive(Debug)]
pub enum Refused {
    Full,
    Stopped,
    Failed(String),
}
impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Full => f.write_str("the writer is busy, try again"),
            Self::Stopped => f.write_str("the writer stopped"),
            Self::Failed(e) => f.write_str(e),
        }
    }
}
/// Queues `job` and waits until the writer has applied it. A full queue is
/// refused at once, so a caller can answer `429`.
pub async fn submit(sender: &Sender, job: Job) -> std::result::Result<(), Refused> {
    let (done, outcome) = oneshot::channel();
    sender.try_send(Write { job, done }).map_err(|e| match e {
        TrySendError::Full(_) => Refused::Full,
        TrySendError::Disconnected(_) => Refused::Stopped,
    })?;
    outcome
        .await
        .map_err(|_closed| Refused::Stopped)?
        .map_err(Refused::Failed)
}
/// Queues `job` and waits until the writer has applied it. A full queue is
/// tried again every 50 ms. Returns without waiting when the writer is gone.
pub async fn queue_and_wait(sender: &Sender, mut job: Job) {
    loop {
        let (done, applied) = oneshot::channel();
        match sender.try_send(Write { job, done }) {
            Ok(()) => {
                drop(applied.await);
                return;
            }
            Err(TrySendError::Full(write)) => {
                job = write.job;
                tokio::time::sleep(QUEUE_FULL_WAIT).await;
            }
            Err(TrySendError::Disconnected(_)) => return,
        }
    }
}

/// Queues a [`Job::Sweep`] every `period`, the first one at once. A full
/// queue skips a tick. The task ends when the writer stops.
pub async fn sweep_every(sender: Sender, period: std::time::Duration) {
    let mut ticks = tokio::time::interval(period);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        ticks.tick().await;
        let (done, _) = oneshot::channel();
        let job = Job::Sweep { at: time::now_ms() };
        if let Err(TrySendError::Disconnected(_)) = sender.try_send(Write { job, done }) {
            return;
        }
    }
}
#[derive(Serialize, Debug)]
pub struct Issue {
    pub id: i64,
    pub status: String,
    pub count: i64,
    pub title: String,
    pub culprit: Option<String>,
}
pub fn issues(c: &Connection, project: Option<&str>) -> Result<Vec<Issue>> {
    let mut q = c.prepare("SELECT i.id,i.status,i.event_count,i.title,i.culprit FROM issues i JOIN projects p ON p.id=i.project_id WHERE (?1 IS NULL OR p.slug=?1) ORDER BY i.last_seen DESC,i.id DESC")?;
    Ok(q.query_map([project], |r| {
        Ok(Issue {
            id: r.get(0)?,
            status: r.get(1)?,
            count: r.get(2)?,
            title: r.get(3)?,
            culprit: r.get(4)?,
        })
    })?
    .collect::<rusqlite::Result<_>>()?)
}
/// The newest event of an issue. It always keeps its payload, older ones only in the sample.
pub fn payload(c: &Connection, issue: i64) -> Result<Option<serde_json::Value>> {
    let Some(b) = c
        .query_row(
            "SELECT payload FROM events WHERE issue_id=?1 AND payload IS NOT NULL ORDER BY id DESC LIMIT 1",
            [issue],
            |r| r.get::<_, Vec<u8>>(0),
        )
        .optional()?
    else {
        return Ok(None);
    };
    Ok(Some(serde_json::from_slice(&zstd::stream::decode_all(
        b.as_slice(),
    )?)?))
}
#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct IssueDetail {
    pub id: i64,
    pub project_id: i64,
    pub project: String,
    pub title: String,
    pub culprit: Option<String>,
    pub level: String,
    pub status: crate::issues::Status,
    pub resolved_in_release: Option<String>,
    pub resolved_at: Option<String>,
    pub regressed_at: Option<String>,
    pub first_seen: String,
    pub last_seen: String,
    #[serde(skip)]
    pub last_seen_ms: Timestamp,
    pub event_count: i64,
}
pub fn issue(c: &Connection, id: i64) -> Result<Option<IssueDetail>> {
    let ts = |ms: Option<Timestamp>| ms.map(time::rfc3339_ms);
    Ok(c.query_row(
        "SELECT i.id,i.project_id,p.slug,i.title,i.culprit,i.level,i.status,i.resolved_in_release,i.resolved_at,i.regressed_at,i.first_seen,i.last_seen,i.event_count FROM issues i JOIN projects p ON p.id=i.project_id WHERE i.id=?1",
        [id],
        |r| {
            Ok(IssueDetail {
                id: r.get(0)?,
                project_id: r.get(1)?,
                project: r.get(2)?,
                title: r.get(3)?,
                culprit: r.get(4)?,
                level: r.get(5)?,
                status: r.get(6)?,
                resolved_in_release: r.get(7)?,
                resolved_at: ts(r.get(8)?),
                regressed_at: ts(r.get(9)?),
                first_seen: time::rfc3339_ms(r.get(10)?),
                last_seen: time::rfc3339_ms(r.get(11)?),
                last_seen_ms: r.get(11)?,
                event_count: r.get(12)?,
            })
        },
    )
    .optional()?)
}
#[derive(Debug, Serialize)]
pub struct EventRow {
    pub event_id: String,
    pub timestamp: String,
    pub release: Option<String>,
    pub environment: Option<String>,
    pub trace_id: Option<String>,
    pub user_id: Option<String>,
}
/// The newest 10 events of an issue.
pub fn recent_events(c: &Connection, issue: i64) -> Result<Vec<EventRow>> {
    let mut q = c.prepare("SELECT event_id,timestamp,release,environment,trace_id,user_id FROM events WHERE issue_id=?1 ORDER BY id DESC LIMIT 10")?;
    Ok(q.query_map([issue], |r| {
        Ok(EventRow {
            event_id: r.get(0)?,
            timestamp: time::rfc3339_ms(r.get(1)?),
            release: r.get(2)?,
            environment: r.get(3)?,
            trace_id: r.get(4)?,
            user_id: r.get(5)?,
        })
    })?
    .collect::<rusqlite::Result<_>>()?)
}
#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct ReleaseCount {
    pub release: String,
    pub events: i64,
    pub last_seen: String,
}
/// The releases an issue's events came from, most recent first.
pub fn issue_releases(c: &Connection, issue: i64) -> Result<Vec<ReleaseCount>> {
    let mut q = c.prepare("SELECT release,count(*),max(timestamp) FROM events WHERE issue_id=?1 AND release IS NOT NULL GROUP BY release ORDER BY max(timestamp) DESC LIMIT 20")?;
    Ok(q.query_map([issue], |r| {
        Ok(ReleaseCount {
            release: r.get(0)?,
            events: r.get(1)?,
            last_seen: time::rfc3339_ms(r.get(2)?),
        })
    })?
    .collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(n: i64) -> (Write, oneshot::Receiver<Outcome>) {
        let (done, outcome) = oneshot::channel();
        let job = Job::Sweep { at: n };
        (Write { job, done }, outcome)
    }

    #[test]
    fn a_job_from_a_person_goes_first_while_ingest_is_full() {
        let (ingest_tx, ingest) = crossbeam_channel::bounded(2);
        let (ui_tx, ui) = crossbeam_channel::bounded(UI_QUEUE);
        for n in 1..=2 {
            ingest_tx.try_send(job(n).0).unwrap();
        }
        assert!(ingest_tx.try_send(job(3).0).is_err(), "ingest is full");
        ui_tx.try_send(job(99).0).unwrap();
        let batch = Queues::new(ui, ingest).next_batch().unwrap();
        let order: Vec<_> = batch
            .iter()
            .map(|w| match w.job {
                Job::Sweep { at } => at,
                _ => 0,
            })
            .collect();
        assert_eq!(order, [99, 1, 2]);
    }

    #[test]
    fn people_who_keep_writing_leave_half_of_each_batch_to_ingest() {
        let (ingest_tx, ingest) = crossbeam_channel::bounded(BATCH);
        let (ui_tx, ui) = crossbeam_channel::bounded(UI_QUEUE);
        for n in 0..64 {
            ui_tx.try_send(job(1000 + n).0).unwrap();
            ingest_tx.try_send(job(n).0).unwrap();
        }
        let batch = Queues::new(ui, ingest).next_batch().unwrap();
        let people = batch
            .iter()
            .filter(|w| matches!(w.job, Job::Sweep { at } if at >= 1000))
            .count();
        assert_eq!((batch.len(), people), (64, 32));
    }
}
