//! What the MCP tool `usage` and the web UI's usage routes share: the last
//! disk walk, and the places for usage reads that may run at once. A read
//! holds a connection of its own and may read the log pattern counters, so
//! an agent in a loop and a page left open together cannot start more.

use super::disk::{self, Disk};
use jiff::civil::Date;
use std::{path::Path, sync::Arc};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Usage reads that may run at once, from MCP and the web UI together.
pub const AT_ONCE: usize = 4;
/// Why a usage read past [`AT_ONCE`] is refused.
pub const BUSY: &str = "too many usage reads at once, ask again in a second";

/// The last disk walk and the places for reads, shared by every clone.
#[derive(Clone)]
pub struct Shared {
    disk: disk::Shared,
    reads: Arc<Semaphore>,
}

impl Default for Shared {
    fn default() -> Self {
        Self {
            disk: disk::Shared::default(),
            reads: Arc::new(Semaphore::new(AT_ONCE)),
        }
    }
}

impl Shared {
    /// What [`disk::Shared::measure`] says about `data`.
    pub async fn measure(&self, data: Arc<Path>, today: Date) -> Result<Disk, String> {
        self.disk.measure(data, today).await
    }

    /// A place among the [`AT_ONCE`] usage reads, or `None` when all are
    /// taken. The read holds it until it answers.
    pub fn permit(&self) -> Option<OwnedSemaphorePermit> {
        Arc::clone(&self.reads).try_acquire_owned().ok()
    }
}
