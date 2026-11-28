//! The log writer thread: it takes batches from the queue, commits every
//! second, runs retention every minute and flushes the pattern counters.

use super::{
    COMMIT_EVERY, LogDays, LogInbox, LogQueue, MAX_RETENTION_DAYS, Overrides, QUEUE_BYTES,
    QUEUE_ROWS, RETENTION_BUDGET, RETENTION_EVERY, RETENTION_RETRY, State, tally,
};
use crate::backup::pause::Gate;
use crate::{
    logs::{LogLevel, LogSource},
    self_report,
    time::now_ns,
};
use anyhow::Result;
use std::{
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::Instant,
};

pub struct LogWriter {
    pub queue: LogQueue,
    pub thread: thread::JoinHandle<()>,
    /// What the writer stored and retention cut, for the usage flusher.
    pub log_days: LogDays,
    /// Where the drop rules are put in force, see src/drops.rs.
    pub drops: crate::drops::Live,
}

impl LogWriter {
    /// Starts the writer of the day indexes in `root`. A project keeps
    /// `retention_days` of logs unless `overrides` names it.
    pub fn start(root: PathBuf, retention_days: u32, overrides: Overrides) -> Result<Self> {
        Self::start_with(root, retention_days, overrides, None)
    }

    /// As [`LogWriter::start`], turning days older than `hot_days` cold,
    /// none when it is `None`. See "Cold tier" in docs/design.md.
    pub fn start_with(
        root: PathBuf,
        retention_days: u32,
        overrides: Overrides,
        hot_days: Option<u32>,
    ) -> Result<Self> {
        anyhow::ensure!(
            (1..=MAX_RETENTION_DAYS).contains(&retention_days),
            "log retention must be 1 to {MAX_RETENTION_DAYS} days"
        );
        std::fs::create_dir_all(&root)?;
        let (queue, inbox) = LogQueue::bounded(QUEUE_ROWS, QUEUE_BYTES);
        if let Some(days) = hot_days {
            anyhow::ensure!(
                (1..=MAX_RETENTION_DAYS).contains(&days),
                "hot days must be 1 to {MAX_RETENTION_DAYS}"
            );
        }
        let gate_root = root.clone();
        let mut state = State::new(root, retention_days, overrides);
        state.cold.hot_days = hot_days;
        let log_days = state.log_days.clone();
        let drops = state.drops.clone();
        let thread = thread::spawn(move || run(state, &inbox, &gate_root));
        Ok(Self {
            queue,
            thread,
            log_days,
            drops,
        })
    }
}

/// The writer's loop, on its own thread: until the queue closes it takes
/// batches, commits, runs retention and the cold tier, and waits for backups
/// between turns.
fn run(mut state: State, inbox: &LogInbox, gate_root: &Path) {
    let mut gate = gate(gate_root);
    report(LogLevel::Warn, "cold tier", state.recover_cold(), false);
    let mut next_commit = Instant::now() + COMMIT_EVERY;
    let mut next_flush = Instant::now() + tally::FLUSH_EVERY;
    let mut next_retention = Instant::now();
    let mut retry = RETENTION_RETRY;
    loop {
        // Retention goes first, so the first batch is settled with
        // each project's own retention.
        if Instant::now() >= next_retention {
            let pass = state.retention(now_ns(), RETENTION_BUDGET);
            let wait = if pass.is_ok() {
                retry = RETENTION_RETRY;
                RETENTION_EVERY
            } else {
                let wait = retry;
                retry = (retry * 2).min(RETENTION_EVERY);
                wait
            };
            next_retention = Instant::now() + wait;
            report(LogLevel::Warn, "retention", pass, false);
        }
        let wait = next_commit.saturating_duration_since(Instant::now());
        match inbox.recv_timeout(wait) {
            Ok((batch, _held)) => {
                let own = batch.iter().all(|r| r.source == LogSource::Davy);
                report(LogLevel::Error, "write", state.add(batch), own);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        let now = Instant::now();
        if now >= next_commit {
            let own = !state.foreign();
            report(LogLevel::Error, "commit", state.commit(), own);
            report(LogLevel::Warn, "close idle", state.close_idle(), false);
            report(
                LogLevel::Warn,
                "cold tier",
                state.cold_tick(now_ns()),
                false,
            );
            next_commit = now + COMMIT_EVERY;
        }
        if now >= next_flush {
            state.flush_tallies(now);
            next_flush = now + tally::FLUSH_EVERY;
        }
        if let Some(gate) = &mut gate {
            report(LogLevel::Warn, "backup pause", gate.check(), false);
        }
    }
    report(LogLevel::Error, "shutdown", state.close_all(), false);
}

/// The backup gate of the log directory `root`, entered. A backup pauses
/// the writer between turns of its loop, where a crash would leave the
/// files as recovery takes them. Without the gate the writer writes on, and
/// a backup refuses the directory.
fn gate(root: &Path) -> Option<Gate> {
    let mut gate = match Gate::open(root) {
        Ok(gate) => gate,
        Err(e) => {
            report(LogLevel::Warn, "backup pause", Err(e), false);
            return None;
        }
    };
    report(LogLevel::Warn, "backup pause", gate.check(), false);
    Some(gate)
}

/// Reports a failure of the log writer. One that lost Davy's own lines and
/// no others goes to stderr only: recording it would write to the index
/// that just failed, and fail again.
fn report(level: LogLevel, what: &str, r: Result<()>, own: bool) {
    if let Err(e) = r {
        let _quiet = own.then(self_report::quiet);
        self_report::log(level, "log writer", format_args!("{what}: {e:#}"));
    }
}

#[cfg(test)]
#[path = "log_writer_tests.rs"]
mod tests;
