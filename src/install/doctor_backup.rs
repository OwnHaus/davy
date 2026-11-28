//! The doctor check `backup`: when `davy backup` last took the data
//! directory, from the record it leaves there. None in [`STALE`] warns,
//! unless the database is younger than that.

use super::{Check, Status, not_ok, ok};
use crate::{backup::Record, store::DB_FILE, text::size};
use std::{path::Path, time::SystemTime};

const NAME: &str = "backup";
/// How old the last backup may be before the check warns.
const STALE_DAYS: i64 = 7;
const DAY_MS: i64 = 86_400_000;

/// The check for the data directory `data`, which `d` names for a shell.
pub(super) fn backup(data: &Path, d: &str, now_ms: i64) -> Check {
    let fix = format!(
        "sudo runuser -u davy -- davy backup --data {d} /var/backups/davy/davy-$(date +%F).tar.zst, and a timer that runs it daily: see Backups in docs/install.md"
    );
    match Record::load(data) {
        Err(e) => not_ok(NAME, Status::Warn, format!("{e:#}"), fix),
        Ok(None) => match age_ms(&data.join(DB_FILE), now_ms) {
            Some(age) if age < STALE_DAYS * DAY_MS => ok(
                NAME,
                format!("no backup taken yet, and the database is {}", days(age)),
            ),
            _ => not_ok(
                NAME,
                Status::Warn,
                format!("davy backup never took {d}"),
                fix,
            ),
        },
        Ok(Some(r)) => {
            let age = now_ms.saturating_sub(r.at_ms);
            let detail = format!(
                "the last backup is {} old: {} of files as {} at {}",
                days(age),
                size(r.bytes),
                size(r.written),
                r.to
            );
            if age < STALE_DAYS * DAY_MS {
                ok(NAME, detail)
            } else {
                not_ok(NAME, Status::Warn, detail, fix)
            }
        }
    }
}

/// How long ago the file at `path` was created, where the file system
/// knows.
fn age_ms(path: &Path, now_ms: i64) -> Option<i64> {
    let born = std::fs::metadata(path).ok()?.created().ok()?;
    let ms = born
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()?
        .as_millis();
    Some(now_ms.saturating_sub(i64::try_from(ms).ok()?))
}

fn days(ms: i64) -> String {
    match ms / DAY_MS {
        0 => "less than a day".to_owned(),
        1 => "1 day".to_owned(),
        n => format!("{n} days"),
    }
}

#[cfg(test)]
#[path = "doctor_backup_tests.rs"]
mod tests;
