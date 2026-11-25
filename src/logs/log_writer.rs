//! The log writer thread: it takes batches from the queue, commits every
//! second, runs retention every minute and flushes the pattern counters.

use super::{
    COMMIT_EVERY, LogDays, LogQueue, MAX_RETENTION_DAYS, Overrides, QUEUE_BYTES, QUEUE_ROWS,
    RETENTION_BUDGET, RETENTION_EVERY, RETENTION_RETRY, State, tally,
};
use crate::time::now_ns;
use anyhow::Result;
use std::{path::PathBuf, sync::mpsc, thread, time::Instant};

pub struct LogWriter {
    pub queue: LogQueue,
    pub thread: thread::JoinHandle<()>,
    /// What the writer stored and retention cut, for the usage flusher.
    pub log_days: LogDays,
}

impl LogWriter {
    /// Starts the writer of the day indexes in `root`. A project keeps
    /// `retention_days` of logs unless `overrides` names it.
    pub fn start(root: PathBuf, retention_days: u32, overrides: Overrides) -> Result<Self> {
        anyhow::ensure!(
            (1..=MAX_RETENTION_DAYS).contains(&retention_days),
            "log retention must be 1 to {MAX_RETENTION_DAYS} days"
        );
        std::fs::create_dir_all(&root)?;
        let (queue, inbox) = LogQueue::bounded(QUEUE_ROWS, QUEUE_BYTES);
        let mut state = State::new(root, retention_days, overrides);
        let log_days = state.log_days.clone();
        let thread = thread::spawn(move || {
            #[expect(
                clippy::print_stderr,
                reason = "the server has no logger yet, stderr is its log"
            )]
            let report = |what: &str, r: Result<()>| {
                if let Err(e) = r {
                    eprintln!("log writer: {what}: {e:#}");
                }
            };
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
                    report("retention", pass);
                }
                let wait = next_commit.saturating_duration_since(Instant::now());
                match inbox.recv_timeout(wait) {
                    Ok((batch, _held)) => report("write", state.add(batch)),
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
                let now = Instant::now();
                if now >= next_commit {
                    report("commit", state.commit());
                    report("close idle", state.close_idle());
                    next_commit = now + COMMIT_EVERY;
                }
                if now >= next_flush {
                    state.flush_tallies(now);
                    next_flush = now + tally::FLUSH_EVERY;
                }
            }
            report("shutdown", state.close_all());
        });
        Ok(Self {
            queue,
            thread,
            log_days,
        })
    }
}
