//! The thread that turns Davy's own lines into log records of the project
//! `davy`. It makes the project on its first line, scrubs the records with
//! the project's policy and queues them like any other logs, counted in the
//! project's usage. Every failure here goes to stderr only.

use super::{LOG_DAYS, Line, PROJECT, SERVICE, quiet, set_project_id, stderr};
use crate::{
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
    time::Duration,
};

/// How long the thread waits for a line before it looks for other work.
pub const TICK: Duration = Duration::from_secs(1);
/// Lines queued as one batch of records at most.
pub const MAX_BATCH: usize = 256;
/// Where failures of the forwarder are printed.
const COMPONENT: &str = "self report";

/// What the forwarder writes to. It ends once every [`super::Sink`] is gone
/// and the lines they queued are written.
pub struct Forwarder {
    pub data: PathBuf,
    pub logs: LogQueue,
    pub usage: usage::Counter,
    pub policies: Arc<scrub::settings::Cache>,
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

    fn run(self, lines: &mpsc::Receiver<Line>) {
        let _quiet = quiet();
        let mut project = None;
        loop {
            let first = match lines.recv_timeout(TICK) {
                Ok(line) => line,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            };
            let mut batch = vec![first];
            batch.extend(lines.try_iter().take(MAX_BATCH - 1));
            self.forward(&mut project, batch);
        }
    }

    /// Writes `lines` to the project `davy`, made or found first if this
    /// process has not yet.
    fn forward(&self, project: &mut Option<Project>, mut lines: Vec<Line>) {
        if project.is_none() {
            match self.project() {
                Ok((found, created)) => {
                    if created {
                        lines.insert(0, created_line(lines[0].at_ns));
                    }
                    set_project_id(found.id);
                    *project = Some(found);
                }
                Err(e) => {
                    let n = lines.len();
                    stderr(
                        COMPONENT,
                        format!(
                            "{n} of Davy's own lines were not recorded, as the project {PROJECT:?} could not be opened: {e:#}"
                        ),
                    );
                    return;
                }
            }
        }
        if let Some(project) = project {
            self.write(project, lines);
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
        let policy = self
            .policies
            .get(&project.db, project.id)
            .unwrap_or_else(|e| {
                stderr(
                    COMPONENT,
                    format!("the scrub policy of project {PROJECT:?} was not read, so the strictest is used: {e:#}"),
                );
                Arc::new(policy())
            });
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
