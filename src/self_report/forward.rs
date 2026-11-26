//! The thread that turns Davy's own lines into log records of Davy's own
//! project, and its crash files into issues. `davy serve` finds or makes
//! the project before its writers start, see [`super::own`]. The thread
//! scrubs with the project's policy and queues like any other ingest,
//! counted in the project's usage. It asks the quota gate like any sender:
//! past the project's log quota its lines are dropped and counted as
//! refused. Every failure here goes to stderr only.

use super::{
    Line, SERVICE,
    crash::{self, Told},
    own::{self, Own},
    quiet, stderr,
};
use crate::{
    http::body::Budget,
    logs::{
        LogRecord, LogSource,
        store::{LogQueue, QueueError},
    },
    scrub::{self, Meter, Policy},
    store,
    usage::{self, Count, Signal},
};
use rusqlite::Connection;
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
    /// Davy's own project.
    pub own: Own,
    /// A connection to read the project's policy and its events with.
    pub db: Mutex<Connection>,
    pub logs: LogQueue,
    pub usage: usage::Counter,
    pub policies: Arc<scrub::settings::Cache>,
    /// Where crashes are stored as issues.
    pub sender: store::Sender,
    pub budget: Budget,
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
        let mut told = Told::default();
        crash::sweep(&crash::dir(&self.data));
        self.crashes(&mut told);
        let mut scanned = Instant::now();
        loop {
            match lines.recv_timeout(TICK) {
                Ok(first) => {
                    let mut batch = vec![first];
                    batch.extend(lines.try_iter().take(MAX_BATCH - 1));
                    self.write(batch);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.crashes(&mut told);
                    return;
                }
            }
            if scanned.elapsed() >= SCAN {
                self.crashes(&mut told);
                scanned = Instant::now();
            }
        }
    }

    /// Records the crash files that wait, as issues of Davy's own project.
    /// An entry that is not a regular file is left alone and named once.
    fn crashes(&self, told: &mut Told) {
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
        if listing.files.is_empty() {
            return;
        }
        let policy = self.policy();
        let target = crash::Target {
            project: self.own.id,
            policy: &policy,
            sender: &self.sender,
            budget: &self.budget,
            usage: &self.usage,
            db: &self.db,
        };
        crash::record(&listing.files, &target, told);
    }

    fn write(&self, lines: Vec<Line>) {
        let now = crate::time::now_ns();
        let gate = self.usage.gate();
        let limits = gate.limits(self.own.id, now);
        if limits.refuses(Signal::Logs) {
            let n = lines.len() as u64;
            let reason = limits.reason(Signal::Logs);
            gate.refuse(self.own.id, reason, "log_item", n, now);
            return;
        }
        let mut records: Vec<LogRecord> = lines
            .into_iter()
            .map(|line| record(self.own.id, line, now))
            .collect();
        let policy = self.policy();
        policy.logs(&mut records, &mut Meter::unmetered());
        let count = Count::new(records.len(), records.iter().map(size).sum());
        let n = records.len();
        match self.logs.push(records) {
            Ok(()) => self.usage.add(self.own.id, Signal::Logs, count),
            Err(e) => stderr(
                COMPONENT,
                format!("{n} of Davy's own lines were not recorded: {}", why(&e)),
            ),
        }
    }

    /// The scrub policy of the project, or [`own::policy`] when it cannot
    /// be read.
    fn policy(&self) -> Arc<Policy> {
        self.policies.get(&self.db, self.own.id).unwrap_or_else(|e| {
            stderr(
                COMPONENT,
                format!(
                    "the scrub policy of project {:?} was not read, so the one Davy sets for it is used: {e:#}",
                    self.own.slug
                ),
            );
            Arc::new(own::policy())
        })
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
        level_unknown: false,
        message: line.message,
        service: Some(SERVICE.to_owned()),
        trace_id: None,
        span_id: None,
        attributes,
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
