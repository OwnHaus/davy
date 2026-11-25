//! The queue between log ingest and the log writer.

use super::LogRecord;
use crate::heap;
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering::SeqCst},
        mpsc,
    },
    time::Duration,
};

pub enum QueueError {
    /// The queue has no room for the batch now.
    Full,
    /// The batch is larger than the whole queue and never fits.
    TooLarge,
    Stopped,
}

/// What the log queue holds, counted in rows and in the heap the records
/// take, from the moment a batch is queued until the writer is done with it.
#[derive(Default)]
struct Load {
    rows: AtomicUsize,
    bytes: AtomicUsize,
}

/// A batch's share of the [`Load`], given back when it is dropped: after the
/// writer has indexed the batch, or when the batch is refused or lost with
/// the channel.
pub struct Held {
    load: Arc<Load>,
    rows: usize,
    bytes: usize,
}

impl Drop for Held {
    fn drop(&mut self) {
        self.load.rows.fetch_sub(self.rows, SeqCst);
        self.load.bytes.fetch_sub(self.bytes, SeqCst);
    }
}

/// Ingest side of the log queue. Bounded in rows and bytes rather than
/// batches, because a batch can hold one record or thousands and memory
/// follows the records.
#[derive(Clone)]
pub struct LogQueue {
    sender: mpsc::Sender<(Vec<LogRecord>, Held)>,
    load: Arc<Load>,
    rows: usize,
    bytes: usize,
}

pub struct LogInbox {
    receiver: mpsc::Receiver<(Vec<LogRecord>, Held)>,
}

impl LogQueue {
    pub fn bounded(rows: usize, bytes: usize) -> (Self, LogInbox) {
        let (sender, receiver) = mpsc::channel();
        let queue = Self {
            sender,
            load: Arc::default(),
            rows,
            bytes,
        };
        (queue, LogInbox { receiver })
    }

    /// Whether `batch` fits now. Other pushes can take the room before
    /// this one's: for a single producer, such as a benchmark, that waits
    /// for room instead of pushing a copy that may be refused.
    pub fn has_room(&self, batch: &[LogRecord]) -> bool {
        let bytes: usize = batch.iter().map(heap::of_record).sum();
        self.load.rows.load(SeqCst) + batch.len() <= self.rows
            && self.load.bytes.load(SeqCst) + bytes <= self.bytes
    }

    pub fn push(&self, batch: Vec<LogRecord>) -> Result<(), QueueError> {
        let rows = batch.len();
        if rows == 0 {
            return Ok(());
        }
        let bytes = batch.iter().map(heap::of_record).sum();
        if rows > self.rows || bytes > self.bytes {
            return Err(QueueError::TooLarge);
        }
        let rows_before = self.load.rows.fetch_add(rows, SeqCst);
        let bytes_before = self.load.bytes.fetch_add(bytes, SeqCst);
        let held = Held {
            load: Arc::clone(&self.load),
            rows,
            bytes,
        };
        if rows_before + rows > self.rows || bytes_before + bytes > self.bytes {
            return Err(QueueError::Full);
        }
        self.sender
            .send((batch, held))
            .map_err(|mpsc::SendError(_)| QueueError::Stopped)
    }
}

impl LogInbox {
    /// The next batch, and its share of the queue to drop once it is written.
    pub fn recv_timeout(
        &self,
        wait: Duration,
    ) -> Result<(Vec<LogRecord>, Held), mpsc::RecvTimeoutError> {
        self.receiver.recv_timeout(wait)
    }

    /// The next batch, giving back its share of the queue at once.
    pub fn try_recv(&self) -> Option<Vec<LogRecord>> {
        self.receiver.try_recv().ok().map(|(batch, _)| batch)
    }
}

#[cfg(test)]
#[path = "queue_tests.rs"]
mod tests;
