//! One disk measurement shared by everyone who asks within a minute. The
//! server answers the MCP tool `usage` from it, so an agent that calls the
//! tool in a loop does not keep the server walking its data directory.

use super::Disk;
use jiff::civil::Date;
use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

/// How long a measurement answers before the next caller walks again.
pub const MAX_AGE: Duration = Duration::from_secs(60);

/// The last measurement, shared by every clone.
#[derive(Clone, Default)]
pub struct Shared(Arc<Mutex<Option<Last>>>);

struct Last {
    at: Instant,
    today: Date,
    disk: Result<Disk, String>,
}

impl Shared {
    /// What [`super::measure`] says about `data`, measured at most once a
    /// minute and once a day: a measurement taken yesterday does not
    /// answer for today, whose log day it counts as closed.
    pub async fn measure(&self, data: Arc<Path>, today: Date) -> Result<Disk, String> {
        self.get(today, Instant::now(), move || {
            super::measure(&data, today).map_err(|e| format!("{e:#}"))
        })
        .await
    }

    /// The last measurement when it is younger than [`MAX_AGE`] at `now`
    /// and of `today`, or else what `walk` measures. One walk runs at a
    /// time: callers that arrive during it wait for it, not for a walk of
    /// their own, and they wait as tasks, holding no thread. The walk runs
    /// on a blocking thread in a task that holds the lock, so a caller that
    /// gives up does not stop it, and the next caller gets its result.
    pub(crate) async fn get(
        &self,
        today: Date,
        now: Instant,
        walk: impl FnOnce() -> Result<Disk, String> + Send + 'static,
    ) -> Result<Disk, String> {
        let mut last = Arc::clone(&self.0).lock_owned().await;
        if let Some(l) = last.as_ref()
            && l.today == today
            && now.saturating_duration_since(l.at) < MAX_AGE
        {
            return l.disk.clone();
        }
        let task = tokio::spawn(async move {
            let disk = tokio::task::spawn_blocking(walk)
                .await
                .unwrap_or_else(|e| Err(format!("the disk walk crashed: {e}")));
            *last = Some(Last {
                at: now,
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
