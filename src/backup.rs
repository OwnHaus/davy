//! `davy backup` and `davy restore`: a consistent copy of a data directory,
//! taken while the server runs, as one file, and that file put back. See
//! "Backup and restore" in docs/design.md.

pub mod archive;
pub mod cli;
mod link;
pub mod pause;
pub mod restore;
pub mod swap;
pub mod take;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// In the data directory: what the last backup took, which `davy doctor`
/// reads. Backups leave it out, so a restored directory has none.
pub const RECORD: &str = "last-backup.json";

/// What [`RECORD`] holds.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct Record {
    /// The moment the backup holds, in milliseconds since the epoch: when
    /// its snapshot of the database was opened, during the pause.
    pub at_ms: i64,
    pub davy_version: String,
    pub schema: u32,
    pub files: usize,
    /// The bytes of the files, before compression.
    pub bytes: u64,
    /// The bytes of the backup written.
    pub written: u64,
    /// The file it was written to, or `-` for stdout.
    pub to: String,
}

impl Record {
    /// Writes the record into `data`, replacing the last one whole.
    pub fn save(&self, data: &Path) -> Result<()> {
        let tmp = data.join(format!(".{RECORD}.tmp-{}", std::process::id()));
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, data.join(RECORD)).context("saving the record of the backup")
    }

    /// The record in `data`, `None` when no backup was taken of it.
    pub fn load(data: &Path) -> Result<Option<Self>> {
        match std::fs::read(data.join(RECORD)) {
            Ok(bytes) => Ok(Some(
                serde_json::from_slice(&bytes).with_context(|| format!("reading {RECORD}"))?,
            )),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("reading {RECORD}")),
        }
    }
}

/// Refuses a data directory `data` the user this runs as does not own. As
/// root, a backup would leave files in it the server cannot open, and a
/// restore would put files there the server cannot read.
#[cfg(unix)]
fn owned(data: &Path, what: &str) -> Result<()> {
    use std::os::unix::fs::MetadataExt as _;
    let owner = std::fs::metadata(data)
        .with_context(|| format!("reading {}", data.display()))?
        .uid();
    let me = nix::unistd::geteuid().as_raw();
    anyhow::ensure!(
        owner == me,
        "{} belongs to uid {owner} and this runs as uid {me}: run davy {what} as the user that runs the server, such as sudo runuser -u davy -- davy {what} ...",
        data.display()
    );
    Ok(())
}

#[cfg(not(unix))]
fn owned(_data: &Path, _what: &str) -> Result<()> {
    Ok(())
}
