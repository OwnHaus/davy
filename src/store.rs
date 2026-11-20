const SCHEMA: &str = "PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;
CREATE TABLE IF NOT EXISTS projects(id INTEGER PRIMARY KEY, slug TEXT UNIQUE NOT NULL, public_key TEXT UNIQUE NOT NULL, created_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS issues(id INTEGER PRIMARY KEY, project_id INTEGER NOT NULL REFERENCES projects(id), grouping_version INTEGER NOT NULL, fingerprint TEXT NOT NULL, title TEXT NOT NULL, culprit TEXT, level TEXT NOT NULL, status TEXT NOT NULL, resolved_in_release TEXT, resolved_at INTEGER, regressed_at INTEGER, first_seen INTEGER NOT NULL, last_seen INTEGER NOT NULL, event_count INTEGER NOT NULL, UNIQUE(project_id, grouping_version, fingerprint));
CREATE TABLE IF NOT EXISTS events(id INTEGER PRIMARY KEY, project_id INTEGER NOT NULL REFERENCES projects(id), issue_id INTEGER NOT NULL REFERENCES issues(id), event_id TEXT NOT NULL, timestamp INTEGER NOT NULL, received_at INTEGER NOT NULL, level TEXT NOT NULL, release TEXT, environment TEXT, trace_id TEXT, user_id TEXT, payload BLOB, UNIQUE(project_id, event_id));
CREATE TABLE IF NOT EXISTS issue_hourly(issue_id INTEGER NOT NULL REFERENCES issues(id), hour INTEGER NOT NULL, count INTEGER NOT NULL, PRIMARY KEY(issue_id, hour));
CREATE INDEX IF NOT EXISTS issues_last_seen ON issues(last_seen DESC);
CREATE INDEX IF NOT EXISTS events_issue ON events(issue_id, id DESC);";
/// Schema changes after the first release, applied in order. `PRAGMA
/// user_version` counts how many ran.
const MIGRATIONS: [&str; 3] = [
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
];
use crate::{
    alert,
    event::{ErrorEvent, Timestamp},
    grouping::group,
    time,
};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;
use std::{path::Path, sync::mpsc, thread};
use tokio::sync::oneshot;

const QUEUE_FULL_WAIT: std::time::Duration = std::time::Duration::from_millis(50);

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
    fn as_str(&self) -> &'static str {
        match self {
            Self::Unresolved => "unresolved",
            Self::Resolved { .. } => "resolved",
            Self::Ignored => "ignored",
        }
    }
}
pub fn open(data: &Path) -> Result<Connection> {
    std::fs::create_dir_all(data)?;
    let mut c = Connection::open(data.join("davy.sqlite"))?;
    c.busy_timeout(std::time::Duration::from_secs(5))?;
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
pub fn project_id(c: &Connection, slug: &str) -> Result<Option<i64>> {
    Ok(c.query_row(
        "SELECT id FROM projects WHERE slug=?1",
        params![slug],
        |r| r.get(0),
    )
    .optional()?)
}
pub fn insert(c: &Connection, project: i64, e: &ErrorEvent) -> Result<()> {
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
    let payload = if count < 50 || (count + 1) % 100 == 0 {
        Some(zstd::stream::encode_all(
            serde_json::to_vec(&e.raw)?.as_slice(),
            1,
        )?)
    } else {
        None
    };
    c.execute("INSERT INTO events(project_id,issue_id,event_id,timestamp,received_at,level,release,environment,trace_id,user_id,payload) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",params![project,id,e.event_id.0,e.timestamp,e.received_at,e.level.as_str(),e.release,e.environment,e.trace_id,e.user_id,payload])?;
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
/// Marks an issue fixed in `release`, or in the next release Davy sees when
/// `release` is `None`. Returns false when there is no such issue.
pub fn resolve(c: &Connection, issue: i64, release: Option<&str>, at: Timestamp) -> Result<bool> {
    let changed = c.execute(
        "UPDATE issues SET status='resolved',resolved_in_release=?2,resolved_at=?3 WHERE id=?1",
        params![issue, release, at],
    )?;
    Ok(changed == 1)
}
pub enum Job {
    Ingest {
        project: i64,
        events: Vec<ErrorEvent>,
    },
    Resolve {
        issue: i64,
        release: Option<String>,
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
        result: std::result::Result<(), String>,
    },
}
pub type Outcome = std::result::Result<(), String>;
pub struct Write {
    pub job: Job,
    pub done: oneshot::Sender<Outcome>,
}
/// Runs one job. The outer error is a database failure that aborts the batch,
/// the inner one a problem with this job alone.
fn apply(c: &Connection, job: &Job) -> Result<Outcome> {
    match job {
        Job::Ingest { project, events } => {
            for e in events {
                insert(c, *project, e)?;
            }
            Ok(Ok(()))
        }
        Job::Resolve { issue, release } => {
            let found = resolve(c, *issue, release.as_deref(), time::now_ms())?;
            Ok(found
                .then_some(())
                .ok_or_else(|| format!("no issue {issue}")))
        }
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
    }
}
pub struct Writer {
    pub sender: mpsc::SyncSender<Write>,
    pub thread: thread::JoinHandle<()>,
}
impl Writer {
    pub fn start(mut c: Connection, capacity: usize) -> Self {
        let (sender, receiver) = mpsc::sync_channel::<Write>(capacity);
        let thread = thread::spawn(move || {
            while let Ok(first) = receiver.recv() {
                let mut batch = vec![first];
                batch.extend(receiver.try_iter().take(63));
                let outcomes = (|| -> Result<Vec<Outcome>> {
                    let tx = c.transaction()?;
                    let outcomes = batch
                        .iter()
                        .map(|w| apply(&tx, &w.job))
                        .collect::<Result<_>>()?;
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
        Self { sender, thread }
    }
}
/// Queues a [`Job::Sweep`] every `period`, the first one at once. A full
/// queue skips a tick. The task ends when the writer stops.
/// Queues `job` and waits until the writer has applied it. A full queue is
/// tried again every 50 ms. Returns without waiting when the writer is gone.
pub async fn queue_and_wait(sender: &mpsc::SyncSender<Write>, mut job: Job) {
    loop {
        let (done, applied) = oneshot::channel();
        match sender.try_send(Write { job, done }) {
            Ok(()) => {
                drop(applied.await);
                return;
            }
            Err(mpsc::TrySendError::Full(write)) => {
                job = write.job;
                tokio::time::sleep(QUEUE_FULL_WAIT).await;
            }
            Err(mpsc::TrySendError::Disconnected(_)) => return,
        }
    }
}

pub async fn sweep_every(sender: mpsc::SyncSender<Write>, period: std::time::Duration) {
    let mut ticks = tokio::time::interval(period);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        ticks.tick().await;
        let (done, _) = oneshot::channel();
        let job = Job::Sweep { at: time::now_ms() };
        if let Err(mpsc::TrySendError::Disconnected(_)) = sender.try_send(Write { job, done }) {
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
/// The newest stored event of an issue. Only a sample of events keep a payload.
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
pub struct IssueDetail {
    pub id: i64,
    pub project_id: i64,
    pub project: String,
    pub title: String,
    pub culprit: Option<String>,
    pub level: String,
    pub status: String,
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
