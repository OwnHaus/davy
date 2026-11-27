//! The cold tier: days past `--hot-days` kept as compressed files, one
//! per day and project, searched by the same queries as the day indexes.
//! See "Cold tier" in docs/design.md.

#[doc(hidden)]
#[path = "cold_bench.rs"]
pub mod bench;
#[path = "cold_block.rs"]
mod block;
#[path = "cold_convert.rs"]
mod convert;
#[path = "cold_file.rs"]
mod file;
#[path = "cold_format.rs"]
mod format;
#[path = "cold_query.rs"]
mod query;
