//! The thread that turns Davy's own lines into log records of the project
//! `davy`, and its crash files into issues. It makes the project on its
//! first line or crash, scrubs with the project's policy and queues like
//! any other ingest, counted in the project's usage. Every failure here
//! goes to stderr only.

use super::{
    LOG_DAYS, Line, PROJECT, SERVICE,
    crash::{self, Told},
    quiet, set_project_id, stderr,
};
use crate::{
    http::body::Budget,
    logs::{
        LogLevel, LogRecord, LogSource,
        store::{LogQueue, QueueError},
    },
    scrub::{self, Meter, Policy},
    store,
    usage::{self, Count, Signal},
};
use rusqlite::{Connection, TransactionBehavior, params};
use serde_json::{Map, Value};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

/// How long the thread waits for a line before it looks for other work.
pub const TICK: Duration = Duration::from_secs(1);
/// Lines queued as one batch of records at most.
pub const MAX_BATCH: usize = 256;
/// How often the thread looks for crash files of a panic this process
/// survived, or of another process.
pub const SCAN: Duration = Duration::from_secs(5);
/// Where failures of the forwarder are printed.
const COMPONENT: &str = "self report";

/// What the forwarder writes to. It ends once every [`super::Sink`] is gone
/// and the lines they queued are written.
pub struct Forwarder {
    pub data: PathBuf,
    pub logs: LogQueue,
    pub usage: usage::Counter,
    pub policies: Arc<scrub::settings::Cache>,
    /// Where crashes are stored as issues.
    pub sender: store::Sender,
    pub budget: Budget,
}

/// The project `davy` and a connection to read its policy with.
struct Project {
    id: i64,
    db: Mutex<Connection>,
}

impl Forwarder {
    /// Starts the thread that reads `lines`.
    pub fn start(self, lines: mpsc::Receiver<Line>) -> std::io::Result<thread::JoinHandle<()>> {
        thread::Builder::new()
            .name("self report".to_owned())
            .spawn(move || self.run(&lines))
    }

    /// Records crash files at once, then lines as they come and crash
    /// files every [`SCAN`], and once more when the last sink is gone.
    fn run(self, lines: &mpsc::Receiver<Line>) {
        let _quiet = quiet();
        let mut project = None;
        let mut told = Told::default();
        crash::sweep(&crash::dir(&self.data));
        self.crashes(&mut project, &mut told);
        let mut scanned = Instant::now();
        loop {
            match lines.recv_timeout(TICK) {
                Ok(first) => {
                    let mut batch = vec![first];
                    batch.extend(lines.try_iter().take(MAX_BATCH - 1));
                    self.forward(&mut project, batch);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.crashes(&mut project, &mut told);
                    return;
                }
            }
            if scanned.elapsed() >= SCAN {
                self.crashes(&mut project, &mut told);
                scanned = Instant::now();
            }
        }
    }

    /// Writes `lines` to the project `davy`, made or found first if this
    /// process has not yet.
    fn forward(&self, project: &mut Option<Project>, mut lines: Vec<Line>) {
        let lost = format!("{} of Davy's own lines were", lines.len());
        let Some(created) = self.open(project, &lost) else {
            return;
        };
        if created {
            lines.insert(0, created_line(lines[0].at_ns));
        }
        if let Some(project) = project {
            self.write(project, lines);
        }
    }

    /// Records the crash files that wait, as issues of the project `davy`,
    /// which is made only when there is one. An entry that is not a
    /// regular file is left alone and named once.
    fn crashes(&self, project: &mut Option<Project>, told: &mut Told) {
        let dir = crash::dir(&self.data);
        let listing = match crash::list(&dir) {
            Ok(listing) => listing,
            Err(e) => {
                if told.first(&dir) {
                    stderr(
                        COMPONENT,
                        format_args!("{} was not read: {e}", dir.display()),
                    );
                }
                return;
            }
        };
        for other in listing.others.iter().filter(|p| told.first(p)) {
            stderr(
                "crash",
                format_args!(
                    "{} is not a regular file, so it is left alone",
                    other.display()
                ),
            );
        }
        let files = listing.files;
        if files.is_empty() {
            return;
        }
        let lost = format!("{} crashes in {} were", files.len(), dir.display());
        let Some(created) = self.open(project, &lost) else {
            return;
        };
        let Some(project) = project.as_ref() else {
            return;
        };
        if created {
            self.write(project, vec![created_line(crate::time::now_ns())]);
        }
        let policy = self.policy(project);
        let target = crash::Target {
            project: project.id,
            policy: &policy,
            sender: &self.sender,
            budget: &self.budget,
            usage: &self.usage,
            db: &project.db,
        };
        crash::record(&files, &target, told);
    }

    /// Makes or finds the project `davy` unless this process has. Some(true)
    /// when it was made now. None, with `lost` and why on stderr, when the
    /// store cannot be opened.
    fn open(&self, slot: &mut Option<Project>, lost: &str) -> Option<bool> {
        if slot.is_some() {
            return Some(false);
        }
        match self.project() {
            Ok((found, created)) => {
                set_project_id(found.id);
                *slot = Some(found);
                Some(created)
            }
            Err(e) => {
                stderr(
                    COMPONENT,
                    format!(
                        "{lost} not recorded, as the project {PROJECT:?} could not be opened: {e:#}"
                    ),
                );
                None
            }
        }
    }

    /// Opens the store and finds the project `davy`, or makes it. True when
    /// it was made.
    fn project(&self) -> anyhow::Result<(Project, bool)> {
        let mut c = store::open(&self.data)?;
        let (id, created) = find_or_create(&mut c)?;
        let project = Project {
            id,
            db: Mutex::new(c),
        };
        Ok((project, created))
    }

    fn write(&self, project: &Project, lines: Vec<Line>) {
        let now = crate::time::now_ns();
        let mut records: Vec<LogRecord> = lines
            .into_iter()
            .map(|line| record(project.id, line, now))
            .collect();
        let policy = self.policy(project);
        policy.logs(&mut records, &mut Meter::unmetered());
        let count = Count::new(records.len(), records.iter().map(size).sum());
        let n = records.len();
        match self.logs.push(records) {
            Ok(()) => self.usage.add(project.id, Signal::Logs, count),
            Err(e) => stderr(
                COMPONENT,
                format!("{n} of Davy's own lines were not recorded: {}", why(&e)),
            ),
        }
    }

    /// The scrub policy of the project, or [`policy`] when it cannot be
    /// read.
    fn policy(&self, project: &Project) -> Arc<Policy> {
        self.policies
            .get(&project.db, project.id)
            .unwrap_or_else(|e| {
                stderr(
                    COMPONENT,
                    format!("the scrub policy of project {PROJECT:?} was not read, so the one Davy sets for it is used: {e:#}"),
                );
                Arc::new(policy())
            })
    }
}

/// Finds the project `davy`, or makes it with [`LOG_DAYS`] of logs and
/// [`policy`]. True when it was made. A project of that name that a person
/// made keeps its own settings.
pub fn find_or_create(c: &mut Connection) -> anyhow::Result<(i64, bool)> {
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(id) = store::project_id(&tx, PROJECT)? {
        return Ok((id, false));
    }
    let (id, _key) = store::create_project(&tx, PROJECT)?;
    tx.execute(
        "UPDATE projects SET log_retention_days=?1 WHERE id=?2",
        params![LOG_DAYS, id],
    )?;
    scrub::settings::set(&tx, id, &policy())?;
    tx.commit()?;
    Ok((id, true))
}

/// The policy of the project `davy`: the default, and also e-mail
/// addresses and personal data, which an alert or a check may name and no
/// operator needs from Davy's own lines.
pub fn policy() -> Policy {
    Policy {
        email: true,
        personal: true,
        ..Policy::default()
    }
}

/// The record of `line` in `project`, observed at `now`.
pub fn record(project: i64, line: Line, now: i64) -> LogRecord {
    let mut attributes = Map::new();
    attributes.insert("component".to_owned(), line.component.into());
    attributes.insert("davy.version".to_owned(), env!("CARGO_PKG_VERSION").into());
    if line.suppressed > 0 {
        attributes.insert("davy.suppressed".to_owned(), line.suppressed.into());
    }
    if line.dropped > 0 {
        attributes.insert("davy.dropped".to_owned(), line.dropped.into());
    }
    LogRecord {
        project_id: project,
        timestamp: line.at_ns,
        observed_at: now,
        source: LogSource::Davy,
        level: line.level,
        message: line.message,
        service: Some(SERVICE.to_owned()),
        trace_id: None,
        span_id: None,
        attributes,
    }
}

/// The line that says the project was made, and how to stop it.
fn created_line(at_ns: i64) -> Line {
    let message = format!(
        "created project {PROJECT:?} for Davy's own warnings, errors and crashes, keeping {LOG_DAYS} days of logs; `davy serve --no-self-report` turns this off"
    );
    stderr(COMPONENT, &message);
    Line {
        at_ns,
        level: LogLevel::Info,
        component: COMPONENT,
        message,
        suppressed: 0,
        dropped: 0,
    }
}

/// The bytes a record counts in usage: its message and its attributes as
/// JSON, as an agent would send them.
fn size(r: &LogRecord) -> usize {
    let attributes = Value::Object(r.attributes.clone()).to_string();
    r.message.len() + attributes.len()
}

fn why(e: &QueueError) -> &'static str {
    match e {
        QueueError::Full => "the log queue is full",
        QueueError::TooLarge => "the batch is larger than the log queue",
        QueueError::Stopped => "the log writer has stopped",
    }
}

#[cfg(test)]
#[path = "forward_tests.rs"]
mod tests;
