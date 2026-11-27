//! What a search or count read of the cold tier, as `search_logs`,
//! `count_logs` and `davy logs` report it.

use super::ColdScan;
use crate::time::rfc3339;
use serde::Serialize;

/// [`ColdScan`] for an agent or a person: times in RFC 3339, the
/// failures only when there are some.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct ColdNote {
    /// The part of the window that lies in cold days read.
    pub cold_from: String,
    pub cold_to: String,
    /// How long reading cold files took.
    pub scan_ms: u64,
    /// Rows decompressed and checked.
    pub rows_read: u64,
    /// When the time budget ran out: logs from the window's start to here
    /// may be missing. Absent when every cold day was read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unscanned_to: Option<String>,
    /// Files, blocks and rows left out because a check failed.
    #[serde(skip_serializing_if = "is_zero")]
    pub corrupt_files: u64,
    #[serde(skip_serializing_if = "is_zero")]
    pub corrupt_blocks: u64,
    #[serde(skip_serializing_if = "is_zero")]
    pub corrupt_rows: u64,
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde passes a reference"
)]
fn is_zero(n: &u64) -> bool {
    *n == 0
}

impl From<&ColdScan> for ColdNote {
    fn from(s: &ColdScan) -> Self {
        Self {
            cold_from: rfc3339(s.from_ns),
            cold_to: rfc3339(s.to_ns),
            scan_ms: s.took_ms,
            rows_read: s.rows,
            unscanned_to: s.unscanned_to_ns.map(rfc3339),
            corrupt_files: s.corrupt_files,
            corrupt_blocks: s.corrupt_blocks,
            corrupt_rows: s.corrupt_rows,
        }
    }
}

impl ColdNote {
    /// One line for a person.
    pub fn line(&self) -> String {
        let mut parts = vec![format!(
            "cold logs from {} to {}: {} rows read in {} ms",
            self.cold_from, self.cold_to, self.rows_read, self.scan_ms
        )];
        if let Some(to) = &self.unscanned_to {
            parts.push(format!(
                "the time budget ran out, so logs up to {to} may be missing"
            ));
        }
        if self.corrupt_blocks + self.corrupt_files > 0 {
            parts.push(format!(
                "{} rows of {} blocks and {} files left out, as they failed a check (davy doctor names them)",
                self.corrupt_rows, self.corrupt_blocks, self.corrupt_files
            ));
        }
        parts.join("; ")
    }
}

#[cfg(test)]
#[path = "cold_note_tests.rs"]
mod tests;
