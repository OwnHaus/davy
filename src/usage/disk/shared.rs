//! One disk measurement shared by everyone who asks within a minute. The
//! server answers the MCP tool `usage` from it, so an agent that calls the
//! tool in a loop does not keep the server walking its data directory.

use super::Disk;
use jiff::civil::Date;
use std::{path::Path, sync::Arc, time::Duration};
use tokio::{sync::Mutex, time::Instant};

/// How long a measurement answers, from the end of its walk, before the
/// next caller walks again.
pub const MAX_AGE: Duration = Duration::from_secs(60);
/// How long a caller waits for a walk. A walk reads the clock between
/// entries only, so one read that hangs, as on a stale network mount,
/// holds it past [`super::MAX_TIME`]; its callers stop waiting here.
pub const MAX_WAIT: Duration = Duration::from_secs(30);

/// The last measurement, shared by every clone.
#[derive(Clone, Default)]
pub struct Shared(Arc<Mutex<Option<Last>>>);

struct Last {
    /// When the walk ended. A walk slower than [`MAX_AGE`] still answers
    /// for a minute after it, so a mount that is always slow gets answers
    /// instead of a new walk for every caller.
    at: Instant,
    today: Date,
    disk: Result<Disk, String>,
}

impl Shared {
    /// What [`super::measure`] says about `data`, measured at most once a
    /// minute and once a day: a measurement taken yesterday does not
    /// answer for today, whose log day it counts as closed.
    pub async fn measure(&self, data: Arc<Path>, today: Date) -> Result<Disk, String> {
        self.get(today, move || {
            super::measure(&data, today).map_err(|e| format!("{e:#}"))
        })
        .await
    }

    /// The last measurement when its walk ended less than [`MAX_AGE`] ago
    /// and it is of `today`, or else what `walk` measures. The clock is
    /// tokio's, so a test with the clock paused moves it. One walk runs at a
    /// time: callers that arrive during it wait for it, not for a walk of
    /// their own, and they wait as tasks, holding no thread. The walk runs
    /// on a blocking thread in a task that holds the lock, so a caller that
    /// gives up, or waits longer than [`MAX_WAIT`], does not stop it, and
    /// the next caller gets its result.
    pub(crate) async fn get(
        &self,
        today: Date,
        walk: impl FnOnce() -> Result<Disk, String> + Send + 'static,
    ) -> Result<Disk, String> {
        tokio::time::timeout(MAX_WAIT, self.last_or_walk(today, walk))
            .await
            .unwrap_or_else(|_| {
                Err(format!(
                    "the disk walk is still running after {} s, ask again later",
                    MAX_WAIT.as_secs()
                ))
            })
    }

    async fn last_or_walk(
        &self,
        today: Date,
        walk: impl FnOnce() -> Result<Disk, String> + Send + 'static,
    ) -> Result<Disk, String> {
        let mut last = Arc::clone(&self.0).lock_owned().await;
        if let Some(l) = last.as_ref()
            && l.today == today
            && Instant::now().saturating_duration_since(l.at) < MAX_AGE
        {
            return l.disk.clone();
        }
        let task = tokio::spawn(async move {
            let disk = tokio::task::spawn_blocking(walk)
                .await
                .unwrap_or_else(|e| Err(format!("the disk walk crashed: {e}")));
            *last = Some(Last {
                at: Instant::now(),
                today,
                disk: disk.clone(),
            });
            disk
        });
        task.await
            .unwrap_or_else(|e| Err(format!("the disk walk crashed: {e}")))
    }
}

#[cfg(test)]
#[path = "shared_tests.rs"]
mod tests;
