//! The single writer: one thread applies every write to SQLite, in
//! batches, from two queues.

use super::{Job, Outcome, Sender, UI_QUEUE, Write, apply, latest::Latest};
use crate::time;
use anyhow::Result;
use crossbeam_channel::{Receiver, TrySendError, select_biased};
use rusqlite::{Connection, TransactionBehavior};
use std::thread;
use tokio::sync::oneshot;

const QUEUE_FULL_WAIT: std::time::Duration = std::time::Duration::from_millis(50);
const BATCH: usize = 64;

/// Runs `batch`: every job in one transaction, but each [`Job::Usage`],
/// [`Job::LogDays`], [`Job::Search`] and [`Job::Quota`] in one of its own,
/// so counting and search history can neither roll back ingest nor be rolled back with it.
/// A database failure fails every job of its transaction, a problem with
/// one job only that job.
pub(crate) fn write_batch(c: &mut Connection, batch: &[Write]) -> Vec<Outcome> {
    let (alone, shared): (Vec<_>, Vec<_>) = batch.iter().enumerate().partition(|(_, w)| {
        matches!(
            w.job,
            Job::Usage(_) | Job::LogDays(_) | Job::Search { .. } | Job::Quota(_)
        )
    });
    let mut outcomes = vec![Ok(()); batch.len()];
    let jobs: Vec<&Job> = shared.iter().map(|(_, w)| &w.job).collect();
    for ((i, _), outcome) in shared.iter().zip(transaction(c, &jobs)) {
        outcomes[*i] = outcome;
    }
    for (i, w) in alone {
        if let Some(outcome) = transaction(c, &[&w.job]).pop() {
            outcomes[i] = outcome;
        }
    }
    outcomes
}

/// Runs `jobs` in one transaction.
fn transaction(c: &mut Connection, jobs: &[&Job]) -> Vec<Outcome> {
    if jobs.is_empty() {
        return Vec::new();
    }
    (|| -> Result<Vec<Outcome>> {
        // IMMEDIATE takes the write lock first, waiting for a CLI that
        // writes: a batch that read first could not wait for it.
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut latest = Latest::default();
        let outcomes = jobs
            .iter()
            .map(|job| apply(&tx, job, &mut latest))
            .collect::<Result<_>>()?;
        latest.keep(&tx)?;
        tx.commit()?;
        Ok(outcomes)
    })()
    .unwrap_or_else(|e| vec![Err(e.to_string()); jobs.len()])
}
pub struct Writer {
    /// Ingest, checks, deliveries and sweeps.
    pub sender: Sender,
    /// Jobs from people in the web UI, [`UI_QUEUE`] deep and taken first.
    pub ui: Sender,
    /// Held while a batch that changes a retention setting is written.
    pub retention: crate::retention::Lock,
    pub thread: thread::JoinHandle<()>,
}
impl Writer {
    pub fn start(mut c: Connection, capacity: usize) -> Self {
        let (sender, ingest) = crossbeam_channel::bounded::<Write>(capacity);
        let (ui, people) = crossbeam_channel::bounded::<Write>(UI_QUEUE);
        let retention = crate::retention::Lock::default();
        let settings = retention.clone();
        let thread = thread::spawn(move || {
            let mut queues = Queues::new(people, ingest);
            while let Some(batch) = queues.next_batch() {
                // Held until the change is committed, so the log writer
                // reads either the old setting before it or the new one.
                let changes = batch.iter().any(|w| w.job.sets_retention());
                let held = changes.then(|| settings.hold());
                let outcomes = write_batch(&mut c, &batch);
                drop(held);
                for (write, outcome) in batch.into_iter().zip(outcomes) {
                    #[expect(
                        clippy::let_underscore_must_use,
                        reason = "a closed receiver means the client hung up, and the batch is written either way"
                    )]
                    let _ = write.done.send(outcome);
                }
            }
        });
        Self {
            sender,
            ui,
            retention,
            thread,
        }
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
    queue(sender, job)?.applied().await
}
/// Queues `job` without waiting, so a caller can act on a queued job even
/// when its own future is dropped before the writer applies it. A full
/// queue is refused at once.
pub fn queue(sender: &Sender, job: Job) -> std::result::Result<Queued, Refused> {
    let (done, outcome) = oneshot::channel();
    sender.try_send(Write { job, done }).map_err(|e| match e {
        TrySendError::Full(_) => Refused::Full,
        TrySendError::Disconnected(_) => Refused::Stopped,
    })?;
    Ok(Queued(outcome))
}
/// A job [`queue`] queued.
pub struct Queued(oneshot::Receiver<Outcome>);
impl Queued {
    /// Waits until the writer has applied the job.
    pub async fn applied(self) -> std::result::Result<(), Refused> {
        self.0
            .await
            .map_err(|_closed| Refused::Stopped)?
            .map_err(Refused::Failed)
    }
    /// Like [`Queued::applied`], for a thread outside the async runtime.
    pub fn applied_blocking(self) -> std::result::Result<(), Refused> {
        self.0
            .blocking_recv()
            .map_err(|_closed| Refused::Stopped)?
            .map_err(Refused::Failed)
    }
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
pub async fn sweep_every(
    sender: Sender,
    period: std::time::Duration,
    error_days: crate::retention::Days,
) {
    let mut ticks = tokio::time::interval(period);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        ticks.tick().await;
        let (done, _) = oneshot::channel();
        let job = Job::Sweep {
            at: time::now_ms(),
            error_days,
        };
        if let Err(TrySendError::Disconnected(_)) = sender.try_send(Write { job, done }) {
            return;
        }
    }
}

#[cfg(test)]
#[path = "writer_tests.rs"]
mod tests;
