//! Retention of cold days: a project's logs go with its file.
//!
//! A segment is written from what retention left of its hot day, so it
//! needs a pass only when a setting changes or the day outlives another
//! project's retention. A day older than every retention goes whole, the
//! directory renamed into `.trash` first, so a search sees all of it or
//! none. A younger day loses the files and pattern counters of each
//! project whose retention it outlived, and goes whole when no file is
//! left. Each day is cut under the retention lock with the settings read
//! under it, as hot days are, and the cut marks the counts the same way.
//!
//! A search that listed a file retention removes before it opens it finds
//! it gone and starts over; one that opened it reads what it held.

use super::{
    super::{State, retention::Cut, tally},
    layout,
    turn::{remove_dir, report},
};
use anyhow::{Context, Result};
use jiff::civil::Date;

impl State {
    /// Deletes what expired at `now` from the cold days. A day that fails
    /// is reported and skipped. Settings that cannot be read stop the
    /// pass.
    pub(in crate::logs::store) fn cold_retention(&mut self, now: i64) -> Result<()> {
        let days = layout::days(&self.root)?;
        self.cold.swept.retain(|day, _| days.contains(day));
        for day in days {
            if self.cold.busy(day) || !self.cold_needs(day, &self.policy.cut(day, now)) {
                continue;
            }
            let lock = self.overrides.lock.clone();
            let held = lock.hold_all().context("taking the retention lock")?;
            self.refresh()?;
            let cut = self.policy.cut(day, now);
            if !self.cold_needs(day, &cut) {
                continue;
            }
            match self.cut_cold(day, &cut) {
                Ok(Left::Files) => {
                    self.mark_cut(day, &cut);
                    self.cold.swept.insert(day, cut);
                }
                Ok(Left::Hot) => self.mark_cut(day, &cut),
                Ok(Left::Nothing) => {}
                Err(e) => report(day, &e),
            }
            drop(held);
        }
        Ok(())
    }

    /// Whether `cut` would delete from the segments of `day` what was not
    /// deleted before.
    fn cold_needs(&self, day: Date, cut: &Cut) -> bool {
        match cut {
            Cut::Only(gone) if gone.is_empty() => false,
            _ => self.cold.swept.get(&day) != Some(cut),
        }
    }

    /// Deletes what `cut` takes from the segments of `day`, and the day
    /// when no file is left.
    fn cut_cold(&mut self, day: Date, cut: &Cut) -> Result<Left> {
        let dir = layout::day_dir(&self.root, day);
        let mut left = 0;
        if !matches!(cut, Cut::AllBut(kept) if kept.is_empty()) {
            for seq in layout::segments(&self.root, day)? {
                let seg = layout::segment_dir(&self.root, day, seq);
                for (project, path) in layout::files(&seg)? {
                    if cuts(cut, project) {
                        layout::remove_file(&path)?;
                    } else {
                        left += 1;
                    }
                }
                tally::file::remove(&seg, cut)?;
                layout::sync_dir(&seg)?;
            }
        }
        if left > 0 {
            return Ok(Left::Files);
        }
        let target = self.to_trash(&dir, day)?;
        self.cold.swept.remove(&day);
        // Searches no longer see the day's segments. A hot directory that
        // late logs made keeps its rows, and the hot pass cuts it. Anything
        // in its place, a symlink too, counts as one: unsure, it marks
        // nothing gone.
        let hot = std::fs::symlink_metadata(self.root.join(day.to_string()));
        let left = if matches!(&hot, Err(e) if e.kind() == std::io::ErrorKind::NotFound) {
            self.mark_gone(day);
            Left::Nothing
        } else {
            Left::Hot
        };
        remove_dir(&target)?;
        Ok(left)
    }
}

/// What a cut left of a day.
enum Left {
    /// Cold files.
    Files,
    /// No cold file, but a hot directory.
    Hot,
    /// Nothing: the day is gone, and marked so.
    Nothing,
}

/// Whether `cut` deletes the logs of `project`.
fn cuts(cut: &Cut, project: u64) -> bool {
    match cut {
        Cut::Only(gone) => gone.contains(&project),
        Cut::AllBut(kept) => !kept.contains(&project),
    }
}

#[cfg(test)]
#[path = "cold_retention_tests.rs"]
mod tests;
