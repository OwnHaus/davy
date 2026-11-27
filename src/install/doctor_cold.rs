//! The doctor check `cold`: what logs take hot and cold, and every cold
//! block's checksum. A block that fails warns: searches skip it and say
//! so, and its rows are lost unless a backup holds the file.

use super::{Check, Status, not_ok, ok};
use crate::logs::store::cold::{self, Verified};
use crate::text::size;
use crate::usage::disk;
use jiff::civil::Date;
use std::path::Path;

const NAME: &str = "cold";

/// The check for the data directory `data`, which `d` names for a shell,
/// with `today` the UTC day still open.
pub(super) fn cold(data: &Path, d: &str, today: Date) -> Check {
    let hot = match disk::measure(data, today) {
        Ok(disk) => format!("hot logs take {}", size(disk.hot())),
        Err(_) => "hot logs take an unknown size".to_owned(),
    };
    match cold::verify(&crate::logs::dir(data)) {
        Err(e) => not_ok(
            NAME,
            Status::Warn,
            format!("{hot}, cold files not read: {e:#}"),
            format!("make {d} readable by the user doctor runs as"),
        ),
        Ok(v) if v.days.is_empty() => ok(NAME, format!("{hot}, no day is cold yet")),
        Ok(v) if v.bad_count == 0 => ok(
            NAME,
            format!("{hot}, {}, every block's checksum matches", tier(&v)),
        ),
        Ok(v) => not_ok(
            NAME,
            Status::Warn,
            format!(
                "{hot}, {}, {} failed the check, which searches skip: {}{}",
                tier(&v),
                plural(v.bad_count, "file or block", "files or blocks"),
                v.bad.join("; "),
                if v.bad_count > v.bad.len() as u64 {
                    "; and more"
                } else {
                    ""
                },
            ),
            "restore each file named from a backup, or delete it to drop its logs; the rest of its day is still searched",
        ),
    }
}

/// The cold tier in a few words.
fn tier(v: &Verified) -> String {
    format!(
        "cold logs take {} in {}, {} in {}",
        size(v.bytes()),
        plural(v.days.len() as u64, "day", "days"),
        plural(v.blocks, "block", "blocks"),
        plural(v.files, "file", "files"),
    )
}

fn plural(n: u64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

#[cfg(test)]
#[path = "doctor_cold_tests.rs"]
mod tests;
