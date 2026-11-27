//! The cold tier: days past `--hot-days` kept as compressed files, one
//! per day and project, searched by the same queries as the day indexes.
//! See "Cold tier" in docs/design.md.

#[path = "cold_block.rs"]
mod block;
#[path = "cold_convert.rs"]
mod convert;
#[path = "cold_file.rs"]
mod file;
#[path = "cold_format.rs"]
mod format;
#[path = "cold_layout.rs"]
mod layout;
#[path = "cold_query.rs"]
mod query;
#[path = "cold_retention.rs"]
mod retention;
#[path = "cold_turn.rs"]
mod turn;
pub(super) use turn::Cold;
#[path = "cold_search.rs"]
mod search;
pub use search::{BUDGET, ColdScan};
#[path = "cold_note.rs"]
mod note;
pub use layout::DIR;

/// Whether `name`, in a cold day directory, is a finished segment: not
/// one being written, `.N.tmp`, nor anything else.
pub fn segment_name(name: &str) -> bool {
    layout::seq_of(name).is_some()
}
pub(super) use layout::segments as cold_segments;
pub use note::ColdNote;
pub(super) use search::{CHANGED, Parts, Scan, days};
#[path = "cold_verify.rs"]
mod verify;
#[cfg(test)]
pub(crate) use verify::cold_day;
pub use verify::{MAX_BAD, Verified, verify};
#[doc(hidden)]
#[path = "cold_bench.rs"]
pub mod bench;

/// The days with a segment, oldest first. A directory a cut emptied, or
/// one with only a segment being written, holds no log of its own.
pub(super) fn cold_days(root: &std::path::Path) -> anyhow::Result<Vec<jiff::civil::Date>> {
    let mut kept = Vec::new();
    for day in layout::days(root)? {
        if !layout::segments(root, day)?.is_empty() {
            kept.push(day);
        }
    }
    Ok(kept)
}
