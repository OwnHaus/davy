//! Turns hot days cold, on the log writer thread, with the work itself
//! on a thread of its own so ingest goes on.
//!
//! A day turns cold in steps, and a crash may stop it after any of them:
//!
//! 1. The writer closes the day, writes its pattern counters and seals it
//!    for segment N: `cold-seal` in the hot day holds N.
//! 2. A converter thread writes segment N into `.N.tmp`, syncing each file
//!    and the directory.
//! 3. The writer, under the retention lock and with the settings read
//!    again, checks that the seal still holds and renames `.N.tmp` to `N`.
//!    From here a reader takes segment N and leaves the hot day.
//! 4. The writer renames the hot day into `.trash` and removes it there.
//!
//! Before the first job, and on each tick until it works, the writer
//! finishes a day whose segment N exists and takes the seal off one whose
//! segment does not, and removes every `.N.tmp`: what a crash left. A day
//! whose hot day cannot be removed is tried again, as below; one that
//! fails otherwise is reported and left out, and the others go on. A
//! failure of step 4 leaves the seal, so readers keep taking the segment,
//! and step 4 is done again a minute later until it works; no other day
//! turns cold before.
//!
//! A log written to a sealed day, or retention deleting from it, takes the
//! seal off, so step 3 drops the segment and the day stays hot. When the
//! seal names a segment that exists, it finishes step 4 first, so the log
//! goes into a new hot day that readers see. Step 4 needs no lock: it
//! deletes only a copy.

use super::super::tally;
use super::{
    super::{RETENTION_EVERY, RETENTION_RETRY, State, days, retention::Cut},
    convert::{self, Settings},
    layout,
};
use crate::{self_report, time::now_ns};
use anyhow::{Context, Result, ensure};
use jiff::civil::Date;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::mpsc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

/// How cold files are written, chosen by the benchmark in
/// docs/storage-benchmark.md.
pub(in crate::logs::store) const SETTINGS: Settings = Settings {
    level: 9,
    block_bytes: 1 << 20,
    dict: false,
};

/// Where a test stops a day turning cold, as a crash right after that
/// step would.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::logs::store) enum Step {
    Sealed,
    Written,
    Renamed,
    Trashed,
}

/// What the writer knows of the cold tier.
pub(in crate::logs::store) struct Cold {
    /// Days younger than this stay hot. `None` keeps every day hot.
    pub(in crate::logs::store) hot_days: Option<u32>,
    job: Option<Job>,
    /// Days that failed to turn cold, left as they are until the server
    /// restarts.
    failed: HashSet<Date>,
    /// Days whose segment is in place and whose hot day step 4 failed to
    /// remove, tried again each minute.
    unfinished: HashSet<Date>,
    /// Whether what a crash left is cleared up. No job starts before.
    recovered: bool,
    /// What retention last deleted from each day's segments.
    pub(in crate::logs::store) swept: HashMap<Date, Cut>,
    next_scan: Instant,
    /// Where to stop, for a test.
    pub(in crate::logs::store) crash: Option<Step>,
    /// Where to hold the next conversion, for a test.
    pub(in crate::logs::store) pause: Option<Pause>,
}

/// A hold on a conversion, for a test: once its files are written, the
/// converter says so on `halfway` and waits for `go`, a minute at most.
pub(in crate::logs::store) struct Pause {
    pub(in crate::logs::store) halfway: mpsc::Sender<()>,
    pub(in crate::logs::store) go: mpsc::Receiver<()>,
}

struct Job {
    day: Date,
    seq: u32,
    stop: Arc<AtomicBool>,
    thread: JoinHandle<Result<()>>,
}

impl Cold {
    /// Whether `day` is turning cold now.
    pub(in crate::logs::store) fn busy(&self, day: Date) -> bool {
        self.job.as_ref().is_some_and(|j| j.day == day)
    }

    pub(in crate::logs::store) fn new(hot_days: Option<u32>) -> Self {
        Self {
            hot_days,
            job: None,
            failed: HashSet::new(),
            unfinished: HashSet::new(),
            recovered: false,
            swept: HashMap::new(),
            next_scan: Instant::now(),
            crash: None,
            pause: None,
        }
    }

    /// Holds the next conversion once its files are written: the receiver
    /// hears when it got there, the sender lets it go on.
    #[cfg(test)]
    pub(in crate::logs::store) fn pause_next(&mut self) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
        let (halfway, there) = mpsc::channel();
        let (go, wait) = mpsc::channel();
        self.pause = Some(Pause { halfway, go: wait });
        (there, go)
    }
}

/// How a job ended.
enum Ended {
    /// The day is cold.
    Cold,
    /// The day stays hot for now: a write or retention got to it first.
    Hot,
    /// The retention lock was held: what was written is dropped and the
    /// day is tried again on the next tick.
    Locked(anyhow::Error),
    /// The segment is in place, but step 4 failed: the seal stays.
    Unfinished(anyhow::Error),
}

impl State {
    /// Finishes or rolls back what a crash left of days turning cold. A
    /// day whose hot day step 4 fails to remove is tried again on each
    /// tick, and one that fails otherwise is left out until the server
    /// restarts; the others go on. Fails, to be tried again, only when the
    /// lock or the list of days cannot be had.
    pub(in crate::logs::store) fn recover_cold(&mut self) -> Result<()> {
        let lock = self.overrides.lock.clone();
        let _held = lock.hold_all().context("taking the retention lock")?;
        for day in days::list(&self.root)? {
            let hot = self.root.join(day.to_string());
            let recovered = match self.finish(day) {
                Ok(true) => Ok(()),
                Ok(false) => layout::remove_seal(&hot),
                Err(e) => Err(e),
            };
            match recovered {
                Ok(()) => {}
                Err(e) if self.cold.unfinished.contains(&day) => {
                    report(day, &e.context("recovering, tried again each minute"));
                }
                Err(e) => {
                    self.cold.failed.insert(day);
                    report(day, &e.context("recovering"));
                }
            }
        }
        // A `.N.tmp` left is only space: the next job for its day starts
        // by removing it.
        for day in layout::days(&self.root)? {
            for seq in layout::temps(&self.root, day)? {
                if let Err(e) = remove_dir(&layout::temp_dir(&self.root, day, seq)) {
                    report(day, &e);
                }
            }
        }
        self.cold.recovered = true;
        Ok(())
    }

    /// Step 4 for a day whose seal names a segment that exists: what a
    /// crash, or a failure after the rename, left. Returns whether it did.
    /// A day whose hot day it fails to remove is tried again each minute.
    fn finish(&mut self, day: Date) -> Result<bool> {
        let in_place = match layout::seal(&self.root.join(day.to_string()))? {
            Some(seq) => layout::segments(&self.root, day)?.contains(&seq),
            None => false,
        };
        if !in_place {
            self.cold.unfinished.remove(&day);
            return Ok(false);
        }
        let trashed = self.trash_hot(day);
        if trashed.is_ok() {
            self.cold.unfinished.remove(&day);
        } else {
            self.cold.unfinished.insert(day);
        }
        trashed.map(|()| true)
    }

    /// Step 4 again for each day a failure left with its segment in place
    /// and its hot day still there. Fails once, for the first day that
    /// fails again, after trying them all.
    fn finish_unfinished(&mut self) -> Result<()> {
        let mut days: Vec<Date> = self.cold.unfinished.iter().copied().collect();
        days.sort_unstable();
        let mut first = None;
        for day in days {
            if let Err(e) = self.finish(day) {
                first.get_or_insert((day, e));
            }
        }
        match first {
            None => Ok(()),
            Some((day, e)) => Err(e.context(format!(
                "finishing {day}, which turned cold; {} day(s) wait for this, and no other day turns cold before",
                self.cold.unfinished.len()
            ))),
        }
    }

    /// Ends a job that finished and starts the next. Called on each
    /// commit tick.
    pub(in crate::logs::store) fn cold_tick(&mut self, now: i64) -> Result<()> {
        if self
            .cold
            .job
            .as_ref()
            .is_some_and(|j| j.thread.is_finished())
        {
            self.end_job()?;
        }
        if self.cold.job.is_some() || Instant::now() < self.cold.next_scan {
            return Ok(());
        }
        if !self.cold.recovered
            && let Err(e) = self.recover_cold()
        {
            self.cold.next_scan = Instant::now() + RETENTION_RETRY;
            return Err(e.context("recovering, before any day turns cold"));
        }
        // A fault that stops step 4 for one day would stop it for the next
        // too, and each would hold its logs twice.
        if let Err(e) = self.finish_unfinished() {
            self.cold.next_scan = Instant::now() + RETENTION_EVERY;
            return Err(e);
        }
        if let Some(day) = self.next_cold(now)? {
            return self.start_job(day);
        }
        self.cold.next_scan = Instant::now() + RETENTION_EVERY;
        Ok(())
    }

    /// The oldest hot day to turn cold at `now`: past the hot days, not
    /// open, not failed before, and with nothing retention would delete.
    fn next_cold(&self, now: i64) -> Result<Option<Date>> {
        let Some(hot_days) = self.cold.hot_days else {
            return Ok(None);
        };
        let today = crate::time::day_of(now);
        let Some(first_hot) = today.checked_sub(jiff::Span::new().days(hot_days)).ok() else {
            return Ok(None);
        };
        Ok(days::list(&self.root)?
            .into_iter()
            .filter(|d| *d < first_hot && !self.open.contains_key(d))
            .filter(|d| !self.cold.failed.contains(d))
            .find(|d| !self.needs(*d, &self.policy.cut(*d, now))))
    }

    /// Steps 1 and 2: seals `day` and starts its conversion. A day whose
    /// seal names a segment that exists only needs step 4, and when that
    /// fails to remove the hot day it is tried again each minute. A day
    /// that fails otherwise is left out until the server restarts.
    pub(in crate::logs::store) fn start_job(&mut self, day: Date) -> Result<()> {
        self.close(day)?;
        let started = self.finish(day).and_then(|finished| {
            if finished {
                Ok(None)
            } else {
                self.seal(day).map(Some)
            }
        });
        match started {
            Ok(Some(seq)) => {
                self.spawn(day, seq);
                Ok(())
            }
            Ok(None) => Ok(()),
            Err(e) if self.cold.unfinished.contains(&day) => {
                Err(e.context(format!("finishing {day}, which turned cold")))
            }
            Err(e) => {
                self.cold.failed.insert(day);
                Err(e.context(format!("turning {day} cold, left as it is")))
            }
        }
    }

    /// Step 1 past the close: the pattern counters written and the seal
    /// for the next segment. Returns the segment.
    fn seal(&mut self, day: Date) -> Result<u32> {
        self.write_tally(day);
        ensure!(
            !self.held_tally(day),
            "its pattern counters could not be saved"
        );
        let seq = layout::segments(&self.root, day)?
            .last()
            .map_or(1, |s| s + 1);
        layout::write_seal(&self.root.join(day.to_string()), seq)?;
        Ok(seq)
    }

    /// Step 2, on a thread of its own.
    fn spawn(&mut self, day: Date, seq: u32) {
        let hot = self.root.join(day.to_string());
        let stop = Arc::new(AtomicBool::new(false));
        let temp = layout::temp_dir(&self.root, day, seq);
        let crashed = self.cold.crash == Some(Step::Sealed);
        let pause = self.cold.pause.take();
        let flag = stop.clone();
        let thread = std::thread::spawn(move || {
            ensure!(!crashed, "stopped after the seal");
            segment(&hot, day, seq, &temp, &flag, pause)
        });
        self.cold.job = Some(Job {
            day,
            seq,
            stop,
            thread,
        });
    }

    fn held_tally(&self, day: Date) -> bool {
        self.tallies
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(&day)
    }

    /// Takes the seal off `day`, before a log is written to it or
    /// retention deletes from it: a job for it drops its segment. A seal
    /// that names a segment that exists has step 4 done first, so what
    /// comes next goes into a new hot day. Returns whether the hot day
    /// went that way.
    pub(in crate::logs::store) fn unseal(&mut self, day: Date) -> Result<bool> {
        let hot = self.root.join(day.to_string());
        if let Some(job) = self.cold.job.as_ref().filter(|j| j.day == day) {
            // The stopped job ends as `Ended::Hot`, which takes the seal
            // off again: a failure here is reported, not fatal.
            job.stop.store(true, Ordering::Relaxed);
            if let Err(e) = layout::remove_seal(&hot) {
                report(day, &e);
            }
            return Ok(false);
        }
        if self
            .finish(day)
            .with_context(|| format!("finishing {day}, which turned cold"))?
        {
            return Ok(true);
        }
        // A seal for no segment leaves the hot day read: it may stay.
        if let Err(e) = layout::remove_seal(&hot) {
            report(day, &e);
        }
        Ok(false)
    }

    /// Waits for the job, then steps 3 and 4. A job that failed, for the
    /// lock or in step 4, is tried again a minute later, not on the next
    /// commit.
    pub(in crate::logs::store) fn end_job(&mut self) -> Result<()> {
        let Some(job) = self.cold.job.take() else {
            return Ok(());
        };
        let ended = self.join(job);
        let pause = if ended.is_ok() {
            Duration::ZERO
        } else {
            RETENTION_EVERY
        };
        self.cold.next_scan = Instant::now() + pause;
        ended
    }

    /// Waits for `job`, then steps 3 and 4.
    fn join(&mut self, job: Job) -> Result<()> {
        let (day, seq) = (job.day, job.seq);
        let written = job
            .thread
            .join()
            .map_err(|_panic| anyhow::anyhow!("the conversion panicked"))
            .and_then(|r| r);
        if [Some(Step::Sealed), Some(Step::Written)].contains(&self.cold.crash) {
            return written;
        }
        // A stopped job ends as it may: the day it was for stays hot.
        let ended = if job.stop.load(Ordering::Relaxed) {
            Ok(Ended::Hot)
        } else {
            written.and_then(|()| self.switch(day, seq))
        };
        match ended {
            Ok(Ended::Cold) => Ok(()),
            Ok(Ended::Hot) => self.roll_back(day, seq),
            // The segment is read and the hot day is not: the seal stays,
            // and each tick, or the next start, does step 4 again.
            Ok(Ended::Unfinished(e)) => {
                self.cold.unfinished.insert(day);
                Err(e.context(format!(
                    "turning {day} cold: segment {seq} is in place, the hot day is left for the next try"
                )))
            }
            Ok(Ended::Locked(e)) => Err(match self.roll_back(day, seq) {
                Ok(()) => e.context(format!("turning {day} cold, left hot for the next tick")),
                Err(left) => e.context(format!("turning {day} cold, then: {left:#}")),
            }),
            Err(e) => {
                self.cold.failed.insert(day);
                Err(match self.roll_back(day, seq) {
                    Ok(()) => e.context(format!("turning {day} cold, left hot")),
                    Err(left) => e.context(format!("turning {day} cold, then: {left:#}")),
                })
            }
        }
    }

    /// Drops what a job for segment `seq` of `day` wrote and takes the
    /// seal off, so the hot day stays as it was.
    fn roll_back(&self, day: Date, seq: u32) -> Result<()> {
        remove_dir(&layout::temp_dir(&self.root, day, seq))?;
        layout::remove_seal(&self.root.join(day.to_string()))
    }

    /// Steps 3 and 4, under the retention lock. A lock that cannot be had
    /// is `Ended::Locked`, and once the rename is done, a failure is
    /// `Ended::Unfinished`, never an error.
    fn switch(&mut self, day: Date, seq: u32) -> Result<Ended> {
        let lock = self.overrides.lock.clone();
        let _held = match lock.hold_all() {
            Ok(held) => held,
            Err(e) => return Ok(Ended::Locked(e.context("taking the retention lock"))),
        };
        self.refresh()?;
        let hot = self.root.join(day.to_string());
        let cut = self.policy.cut(day, now_ns());
        if layout::seal(&hot)? != Some(seq) || self.needs(day, &cut) || self.open.contains_key(&day)
        {
            return Ok(Ended::Hot);
        }
        let dir = layout::segment_dir(&self.root, day, seq);
        std::fs::rename(layout::temp_dir(&self.root, day, seq), &dir)?;
        Ok(match self.after_rename(day, seq) {
            Ok(()) => Ended::Cold,
            Err(e) => Ended::Unfinished(e),
        })
    }

    /// What follows the rename: the directories synced, so the segment is
    /// there after a power cut, then step 4.
    fn after_rename(&mut self, day: Date, seq: u32) -> Result<()> {
        layout::sync_dir(&layout::day_dir(&self.root, day))?;
        layout::sync_dir(&layout::cold_root(&self.root))?;
        layout::sync_dir(&self.root)?;
        if self.cold.crash == Some(Step::Renamed) {
            return Ok(());
        }
        let swept = self.swept.get(&day).cloned();
        if self.cold.swept.get(&day) != swept.as_ref() {
            self.cold.swept.remove(&day);
        }
        if layout::segments(&self.root, day)? == [seq]
            && let Some(cut) = swept
        {
            self.cold.swept.insert(day, cut);
        }
        self.trash_hot(day)
    }

    /// Step 4: the hot day, whose logs a segment holds, goes.
    fn trash_hot(&mut self, day: Date) -> Result<()> {
        self.drop_tally(day);
        self.swept.remove(&day);
        self.uncut.remove(&day);
        days::forget(&self.root, day);
        let target = self.to_trash(&self.root.join(day.to_string()), day)?;
        if self.cold.crash == Some(Step::Trashed) {
            return Ok(());
        }
        remove_dir(&target)
    }

    /// Renames `dir` of `day` into `.trash`, where it is removed, and
    /// returns where it went.
    pub(in crate::logs::store) fn to_trash(&self, dir: &Path, day: Date) -> Result<PathBuf> {
        let trash = self.root.join(".trash");
        std::fs::create_dir_all(&trash)?;
        ensure!(
            super::super::day_dirs::real_dir(&trash)?,
            "{} is gone",
            trash.display()
        );
        ensure!(
            super::super::day_dirs::real_dir(dir)?,
            "{} is gone",
            dir.display()
        );
        let target = trash.join(format!("{day}-{}", now_ns()));
        std::fs::rename(dir, &target)?;
        Ok(target)
    }

    /// Stops the job, for a server shutting down, and rolls it back.
    pub(in crate::logs::store) fn stop_cold(&mut self) -> Result<()> {
        let Some(job) = self.cold.job.as_ref() else {
            return Ok(());
        };
        job.stop.store(true, Ordering::Relaxed);
        let day = job.day;
        self.unseal(day)?;
        self.end_job()
    }

    /// Turns `day` cold now, waiting for the conversion: ending the job
    /// joins its thread. For tests.
    #[cfg(test)]
    pub(in crate::logs::store) fn turn_cold(&mut self, day: Date) -> Result<()> {
        self.start_job(day)?;
        self.end_job()
    }
}

/// Step 2: writes the segment `seq` of the hot day at `hot` into `temp`,
/// the logs and the pattern counters, and syncs it.
fn segment(
    hot: &Path,
    day: Date,
    seq: u32,
    temp: &Path,
    stop: &AtomicBool,
    pause: Option<Pause>,
) -> Result<()> {
    if let Some(parent) = temp.parent() {
        std::fs::create_dir_all(parent)?;
    }
    remove_dir(temp)?;
    std::fs::create_dir(temp)?;
    let name = |project| layout::file(temp, project);
    let stopped = || stop.load(Ordering::Relaxed);
    convert::day(hot, day, seq, SETTINGS, &name, &stopped)?;
    let (projects, _aside) = tally::file::projects(hot)?;
    for project in projects {
        // A file that cannot be read is left out, as a report leaves it out.
        if let Ok(Some(counters)) = tally::file::load(hot, project) {
            let bytes = tally::file::encode(&counters)?;
            write_synced(&temp.join(tally::file::name(project)), &bytes)?;
        }
    }
    if let Some(pause) = pause {
        // The test may be gone, and then the conversion goes on.
        let _sent = pause.halfway.send(());
        let _go = pause.go.recv_timeout(Duration::from_secs(60));
    }
    ensure!(!stopped(), "the conversion of {day} was stopped");
    layout::sync_dir(temp)
}

fn write_synced(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut f = std::fs::File::options()
        .write(true)
        .create_new(true)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(path)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    Ok(())
}

/// Removes the directory at `dir` and what it holds, if it is there. A
/// symlink in its place is an error, not followed.
pub(in crate::logs::store) fn remove_dir(dir: &Path) -> Result<()> {
    if super::super::day_dirs::real_dir(dir)? {
        std::fs::remove_dir_all(dir).with_context(|| format!("removing {}", dir.display()))?;
    }
    Ok(())
}

pub(in crate::logs::store) fn report(day: Date, e: &anyhow::Error) {
    self_report::warn("log writer", format_args!("cold tier, {day}: {e:#}"));
}

#[cfg(test)]
#[path = "cold_turn_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "cold_turn_fail_tests.rs"]
mod fail_tests;
