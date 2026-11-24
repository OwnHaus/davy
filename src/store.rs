/// The schema of the first release. It runs on every open, before and outside
/// the migration lock, so it never changes: new tables and indexes go into
/// `MIGRATIONS`.
const SCHEMA: &str = "PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;
CREATE TABLE IF NOT EXISTS projects(id INTEGER PRIMARY KEY, slug TEXT UNIQUE NOT NULL, public_key TEXT UNIQUE NOT NULL, created_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS issues(id INTEGER PRIMARY KEY, project_id INTEGER NOT NULL REFERENCES projects(id), grouping_version INTEGER NOT NULL, fingerprint TEXT NOT NULL, title TEXT NOT NULL, culprit TEXT, level TEXT NOT NULL, status TEXT NOT NULL, resolved_in_release TEXT, resolved_at INTEGER, regressed_at INTEGER, first_seen INTEGER NOT NULL, last_seen INTEGER NOT NULL, event_count INTEGER NOT NULL, UNIQUE(project_id, grouping_version, fingerprint));
CREATE TABLE IF NOT EXISTS events(id INTEGER PRIMARY KEY, project_id INTEGER NOT NULL REFERENCES projects(id), issue_id INTEGER NOT NULL REFERENCES issues(id), event_id TEXT NOT NULL, timestamp INTEGER NOT NULL, received_at INTEGER NOT NULL, level TEXT NOT NULL, release TEXT, environment TEXT, trace_id TEXT, user_id TEXT, payload BLOB, UNIQUE(project_id, event_id));
CREATE TABLE IF NOT EXISTS issue_hourly(issue_id INTEGER NOT NULL REFERENCES issues(id), hour INTEGER NOT NULL, count INTEGER NOT NULL, PRIMARY KEY(issue_id, hour));
CREATE INDEX IF NOT EXISTS events_issue ON events(issue_id, id DESC);";
mod latest;
mod migrate;
mod migrations;
mod writer;
use crate::{
    alert,
    event::{ErrorEvent, Timestamp},
    grouping::group,
    time,
};
use anyhow::Result;
use latest::Latest;
use migrations::MIGRATIONS;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;
use std::path::Path;
use tokio::sync::oneshot;
#[cfg(test)]
pub(crate) use writer::write_batch;
pub use writer::{Refused, Writer, queue_and_wait, submit, sweep_every};

/// The queue of jobs from people in the web UI, apart from ingest, so a full
/// ingest queue does not refuse a login.
pub const UI_QUEUE: usize = 64;

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
/// The `user_version` of a database with every migration run.
#[expect(
    clippy::cast_possible_truncation,
    reason = "there are a few dozen migrations, not 2^32"
)]
pub const SCHEMA_VERSION: u32 = MIGRATIONS.len() as u32;
/// How long a connection waits for another one's write lock.
pub const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Opens the database in `data`, and creates a missing `data` readable by
/// its owner only, because it holds events, logs and keys. Pending migrations
/// run only while no other process has the database open, see
/// `docs/design.md`, "Migrations".
pub fn open(data: &Path) -> Result<Connection> {
    let mut dir = std::fs::DirBuilder::new();
    dir.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut dir, 0o700);
    dir.create(data)?;
    let mut c = migrate::open(&data.join(DB_FILE), migrate::WAIT)?;
    init(&mut c)?;
    Ok(c)
}
/// Creates the schema and runs pending migrations. The server and a CLI
/// process may open the same directory at once, so the version is read inside
/// a write transaction.
pub fn init(c: &mut Connection) -> Result<()> {
    refuse_newer(c.query_row("PRAGMA user_version", [], |r| r.get(0))?)?;
    c.execute_batch(SCHEMA)?;
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let done: u32 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    refuse_newer(done)?;
    for (version, sql) in (1_u32..).zip(MIGRATIONS).filter(|(v, _)| *v > done) {
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", version)?;
    }
    tx.commit()?;
    Ok(())
}
/// Refuses a store that a newer Davy migrated past `MIGRATIONS`. This one
/// would ignore what those migrations added, such as the projects' own
/// retention and `id_floor`, and could delete logs a project keeps or give
/// a new issue the id of a deleted one.
fn refuse_newer(done: u32) -> Result<()> {
    let known = MIGRATIONS.len();
    anyhow::ensure!(
        usize::try_from(done).is_ok_and(|done| done <= known),
        "a newer Davy has upgraded this data directory to schema version {done}, and this one knows versions up to {known}. Run that release or a newer one, or restore the backup taken before the upgrade"
    );
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
    // An id is never used twice, so a link sent for an issue that expired
    // does not lead to another, see src/retention/errors.rs.
    let created = c.execute("INSERT INTO issues(id,project_id,grouping_version,fingerprint,title,culprit,level,status,first_seen,last_seen,event_count) VALUES (max(coalesce((SELECT max(id) FROM issues),0),coalesce((SELECT id FROM id_floor WHERE name='issues'),0))+1,?1,?2,?3,?4,?5,?6,'unresolved',?7,?7,0) ON CONFLICT(project_id,grouping_version,fingerprint) DO NOTHING", params![project,g.version,g.fingerprint,g.title,g.culprit,e.level.as_str(),e.timestamp])? == 1;
    // An issue with no event left waits only for its feedback to be
    // unlinked, so the error starts it again as a new issue would start.
    let revived = !created && c.execute("UPDATE issues SET title=?4,culprit=?5,level=?6,status='unresolved',resolved_in_release=NULL,resolved_at=NULL,regressed_at=NULL,first_seen=?7,last_seen=?7 WHERE project_id=?1 AND grouping_version=?2 AND fingerprint=?3 AND event_count=0", params![project,g.version,g.fingerprint,g.title,g.culprit,e.level.as_str(),e.timestamp])? == 1;
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
    if created || revived || regressed.is_some() {
        let issue = alert::Issue {
            id,
            title,
            culprit,
            level: e.level.as_str().to_owned(),
            release: e.release.clone(),
        };
        let notice = if created || revived {
            alert::Notice::IssueNew(issue)
        } else {
            alert::Notice::IssueRegressed(issue)
        };
        alert::store::notify(c, project, &notice, e.received_at)?;
    }
    Ok(())
}
pub enum Job {
    Scrub {
        project: i64,
        policy: crate::scrub::Policy,
    },
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
    /// Alerts for late monitors, and deleting what expired by `at`. A
    /// project without a retention of its own keeps errors `error_days`.
    Sweep {
        at: Timestamp,
        error_days: crate::retention::Days,
    },
    /// New retention settings for one project, asked by a person or an
    /// agent.
    SetRetention(crate::retention::Change, crate::retention::By),
    /// The result of one HTTP check that started at `at`.
    Check {
        monitor: i64,
        at: Timestamp,
        checked: crate::monitor::check::Checked,
    },
    /// The result of one attempt to send a notification.
    Delivery {
        /// The rows one send covered: one, or every row of a digest.
        ids: Vec<i64>,
        at: Timestamp,
        result: Outcome,
    },
    /// Client reports, user feedback and sessions from one envelope.
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
    /// A check-in from a Sentry SDK, or why it was refused.
    CheckIn {
        project: i64,
        at: Timestamp,
        item: crate::monitor::checkin::Item,
    },
    /// The first time Davy saw `what`. A later one changes nothing.
    First {
        what: crate::firsts::First,
        at: Timestamp,
    },
    /// Records and bytes ingest accepted, see src/usage.rs.
    Usage(Vec<crate::usage::Row>),
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
        Job::Sweep { at, error_days } => {
            crate::monitor::store::sweep(c, *at)?;
            crate::retention::errors::sweep(c, *at, *error_days, crate::retention::errors::BUDGET)?;
            crate::accounts::store::sweep(c, *at)?;
            crate::items::client_report::prune(c, *at)?;
            crate::items::session::prune(c, *at)?;
            crate::usage::prune(c, *at)?;
            Ok(Ok(()))
        }
        Job::Usage(rows) => {
            crate::usage::write(c, rows)?;
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
        Job::Delivery { ids, at, result } => {
            alert::store::delivered(c, ids, *at, result)?;
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
        Job::Scrub { project, policy } => {
            let found = crate::scrub::settings::set(c, *project, policy)?;
            Ok(found
                .then_some(())
                .ok_or_else(|| format!("no project {project}")))
        }
        Job::Account(change) => crate::accounts::store::apply(c, change),
        Job::SetRetention(change, by) => Ok(crate::retention::set(c, change, *by)?.map(drop)),
        Job::First { what, at } => {
            crate::firsts::record(c, *what, *at)?;
            Ok(Ok(()))
        }
        Job::CheckIn { project, at, item } => {
            crate::monitor::checkin_store::apply(c, *project, *at, item)?;
            Ok(Ok(()))
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
#[path = "store_tests.rs"]
mod tests;
