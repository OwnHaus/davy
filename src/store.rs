pub(crate) const SCHEMA: &str = "PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;
CREATE TABLE IF NOT EXISTS projects(id INTEGER PRIMARY KEY, slug TEXT UNIQUE NOT NULL, public_key TEXT UNIQUE NOT NULL, created_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS issues(id INTEGER PRIMARY KEY, project_id INTEGER NOT NULL REFERENCES projects(id), grouping_version INTEGER NOT NULL, fingerprint TEXT NOT NULL, title TEXT NOT NULL, culprit TEXT, level TEXT NOT NULL, status TEXT NOT NULL, resolved_in_release TEXT, resolved_at INTEGER, regressed_at INTEGER, first_seen INTEGER NOT NULL, last_seen INTEGER NOT NULL, event_count INTEGER NOT NULL, UNIQUE(project_id, grouping_version, fingerprint));
CREATE TABLE IF NOT EXISTS events(id INTEGER PRIMARY KEY, project_id INTEGER NOT NULL REFERENCES projects(id), issue_id INTEGER NOT NULL REFERENCES issues(id), event_id TEXT NOT NULL, timestamp INTEGER NOT NULL, received_at INTEGER NOT NULL, level TEXT NOT NULL, release TEXT, environment TEXT, trace_id TEXT, user_id TEXT, payload BLOB, UNIQUE(project_id, event_id));
CREATE TABLE IF NOT EXISTS issue_hourly(issue_id INTEGER NOT NULL REFERENCES issues(id), hour INTEGER NOT NULL, count INTEGER NOT NULL, PRIMARY KEY(issue_id, hour));
CREATE INDEX IF NOT EXISTS issues_last_seen ON issues(last_seen DESC);
CREATE INDEX IF NOT EXISTS events_issue ON events(issue_id, id DESC);";
use crate::{
    event::{ErrorEvent, Timestamp, now},
    grouping::group,
};
use anyhow::Result;
use rusqlite::{Connection, params};
use serde::Serialize;
use std::{path::Path, sync::mpsc, thread};
use tokio::sync::oneshot;

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
    pub fn on_event(&mut self, at: Timestamp) -> Option<Timestamp> {
        match self {
            Self::Resolved { .. } => {
                *self = Self::Unresolved;
                Some(at)
            }
            _ => None,
        }
    }
}
pub fn open(data: &Path) -> Result<Connection> {
    std::fs::create_dir_all(data)?;
    let c = Connection::open(data.join("davy.sqlite"))?;
    c.busy_timeout(std::time::Duration::from_secs(5))?;
    c.execute_batch(SCHEMA)?;
    Ok(c)
}
pub fn create_project(c: &Connection, slug: &str) -> Result<(i64, String)> {
    let key = uuid::Uuid::new_v4().simple().to_string();
    c.execute(
        "INSERT INTO projects(slug,public_key,created_at) VALUES (?1,?2,?3)",
        params![slug, key, now()],
    )?;
    Ok((c.last_insert_rowid(), key))
}
pub fn authenticate(c: &Connection, project: i64, key: &str) -> Result<bool> {
    Ok(c.query_row(
        "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1 AND public_key=?2)",
        params![project, key],
        |r| r.get(0),
    )?)
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
    c.execute("INSERT INTO issues(project_id,grouping_version,fingerprint,title,culprit,level,status,first_seen,last_seen,event_count) VALUES (?1,?2,?3,?4,?5,?6,'unresolved',?7,?7,0) ON CONFLICT(project_id,grouping_version,fingerprint) DO NOTHING", params![project,g.version,g.fingerprint,g.title,g.culprit,e.level.as_str(),e.timestamp])?;
    let (id,count,status,release,at): (i64,i64,String,Option<String>,Option<i64>) = c.query_row("SELECT id,event_count,status,resolved_in_release,resolved_at FROM issues WHERE project_id=?1 AND grouping_version=?2 AND fingerprint=?3", params![project,g.version,g.fingerprint], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
    let mut status = match status.as_str() {
        "resolved" => IssueStatus::Resolved {
            in_release: release,
            at: at.unwrap_or(0),
        },
        "ignored" => IssueStatus::Ignored,
        _ => IssueStatus::Unresolved,
    };
    let regressed = status.on_event(e.received_at);
    let status = match status {
        IssueStatus::Unresolved => "unresolved",
        IssueStatus::Resolved { .. } => "resolved",
        IssueStatus::Ignored => "ignored",
    };
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
    Ok(())
}
pub struct Ingest {
    pub project: i64,
    pub events: Vec<ErrorEvent>,
    pub done: oneshot::Sender<std::result::Result<(), String>>,
}
pub struct Writer {
    pub sender: mpsc::SyncSender<Ingest>,
    pub thread: thread::JoinHandle<()>,
}
impl Writer {
    pub fn start(mut c: Connection, capacity: usize) -> Self {
        let (sender, receiver) = mpsc::sync_channel::<Ingest>(capacity);
        let thread = thread::spawn(move || {
            while let Ok(first) = receiver.recv() {
                let mut batch = vec![first];
                batch.extend(receiver.try_iter().take(63));
                let result = (|| -> Result<()> {
                    let tx = c.transaction()?;
                    for request in &batch {
                        for e in &request.events {
                            insert(&tx, request.project, e)?;
                        }
                    }
                    tx.commit()?;
                    Ok(())
                })()
                .map_err(|e| e.to_string());
                for request in batch {
                    let _ = request.done.send(result.clone());
                }
            }
        });
        Self { sender, thread }
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
pub fn payload(c: &Connection, issue: i64) -> Result<serde_json::Value> {
    let b: Vec<u8> = c.query_row("SELECT payload FROM events WHERE issue_id=?1 AND payload IS NOT NULL ORDER BY id DESC LIMIT 1",[issue],|r|r.get(0))?;
    Ok(serde_json::from_slice(&zstd::stream::decode_all(
        b.as_slice(),
    )?)?)
}
