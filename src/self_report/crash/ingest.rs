//! How the forwarder records crash files: which files wait, how one is
//! read without blocking on what is not a file, and what becomes of it.
//! A file is deleted only once the store has its event. One that cannot
//! be read or parsed is set aside in DATA/crashes/refused, and one the
//! store does not take waits for the next scan.

use super::{FORMAT, MAX_FILE, MAX_FRAMES, MAX_PENDING, PER_SCAN, Report, event, stderr};
use crate::{
    event::{ErrorEvent, EventId},
    http::body::Budget,
    scrub::{Item, Meter, Policy},
    store::{self, Job, Refused},
    usage::{self, Count, Signal},
};
use rusqlite::{Connection, OptionalExtension as _, params};
use std::{
    collections::HashSet,
    fs,
    io::{self, Read as _},
    path::{Path, PathBuf},
    sync::{Mutex, PoisonError},
    time::Duration,
};

/// Directory entries read at most per scan, so a directory filled by
/// mistake costs a bounded read.
pub const MAX_ENTRIES: usize = 1024;
/// The directory of DATA/crashes for files that cannot be recorded.
pub const REFUSED: &str = "refused";
/// A hidden file of a write that did not finish is removed once it is
/// this old, so a write in progress is left alone.
pub const STALE_TMP: Duration = Duration::from_secs(60);
/// Names whose problem is remembered, so each is printed once.
const MAX_TOLD: usize = 1024;
const COMPONENT: &str = "crash";

/// The entries of DATA/crashes.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Listing {
    /// The crash files, the oldest first.
    pub files: Vec<PathBuf>,
    /// Entries named like a crash file that are not regular files, such as
    /// a directory, a symbolic link or a FIFO. They are left alone.
    pub others: Vec<PathBuf>,
}

/// The entries of `dir` named like crash files, up to [`MAX_ENTRIES`]
/// read. Hidden entries are skipped. A missing directory has none.
pub fn list(dir: &Path) -> io::Result<Listing> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Listing::default()),
        Err(e) => return Err(e),
    };
    let mut listing = Listing::default();
    for entry in entries.take(MAX_ENTRIES) {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.as_encoded_bytes();
        if name.starts_with(b".") || !name.ends_with(b".json") {
            continue;
        }
        // The type of the entry itself: a symbolic link is not followed.
        if entry.file_type()?.is_file() {
            listing.files.push(entry.path());
        } else {
            listing.others.push(entry.path());
        }
    }
    listing.files.sort();
    listing.others.sort();
    Ok(listing)
}

/// The crash files in `dir`, the oldest first.
pub fn pending(dir: &Path) -> io::Result<Vec<PathBuf>> {
    list(dir).map(|l| l.files)
}

/// Reads a crash file. Anything but a regular file of this process's user
/// is refused without waiting on it, and so is a file larger than
/// [`MAX_FILE`], in another format or with more than [`MAX_FRAMES`]
/// frames.
pub fn read(path: &Path) -> anyhow::Result<Report> {
    let mut bytes = Vec::new();
    open(path)?
        .take(MAX_FILE as u64 + 1)
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() <= MAX_FILE, "larger than {MAX_FILE} bytes");
    let report: Report = serde_json::from_slice(&bytes)?;
    anyhow::ensure!(
        report.format == FORMAT,
        "format {} is not {FORMAT}",
        report.format
    );
    anyhow::ensure!(
        report.frames.len() <= MAX_FRAMES,
        "more than {MAX_FRAMES} frames"
    );
    EventId::parse(&report.event_id)?;
    Ok(report)
}

/// Opens `path` for reading without following a symbolic link and without
/// waiting for a writer, as a FIFO would make it, then checks that it is a
/// regular file of this process's user.
fn open(path: &Path) -> anyhow::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use nix::fcntl::OFlag;
        std::os::unix::fs::OpenOptionsExt::custom_flags(
            &mut options,
            (OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK).bits(),
        );
    }
    let file = options.open(path)?;
    let meta = file.metadata()?;
    anyhow::ensure!(meta.is_file(), "not a regular file");
    #[cfg(unix)]
    {
        let owner = std::os::unix::fs::MetadataExt::uid(&meta);
        let me = nix::unistd::geteuid().as_raw();
        anyhow::ensure!(
            owner == me,
            "owned by user {owner}, not by this process's user {me}"
        );
    }
    Ok(file)
}

/// Removes the hidden files of writes that did not finish, left by a kill
/// between the write and the rename, once they are [`STALE_TMP`] old.
pub fn sweep(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.take(MAX_ENTRIES).flatten() {
        let name = entry.file_name();
        let name = name.as_encoded_bytes();
        let stale = entry
            .metadata()
            .ok()
            .filter(fs::Metadata::is_file)
            .and_then(|m| m.modified().ok())
            .and_then(|at| at.elapsed().ok())
            .is_some_and(|age| age >= STALE_TMP);
        if name.starts_with(b".") && name.ends_with(b".tmp") && stale {
            match fs::remove_file(entry.path()) {
                Ok(()) => stderr(
                    COMPONENT,
                    format_args!(
                        "removed {}, a crash file never finished",
                        entry.path().display()
                    ),
                ),
                Err(e) => stderr(
                    COMPONENT,
                    format_args!("{} was not removed: {e}", entry.path().display()),
                ),
            }
        }
    }
}

/// The names whose problem was printed, so a file that waits or cannot be
/// moved is printed once and not on every scan.
#[derive(Debug, Default)]
pub struct Told(HashSet<PathBuf>);

impl Told {
    /// True the first time `path` is told. Past [`MAX_TOLD`] names the
    /// rest are not printed.
    pub fn first(&mut self, path: &Path) -> bool {
        self.0.len() < MAX_TOLD && self.0.insert(path.to_owned())
    }
}

/// Where the forwarder records crash files.
pub struct Target<'a> {
    pub project: i64,
    pub policy: &'a Policy,
    pub sender: &'a store::Sender,
    pub budget: &'a Budget,
    pub usage: &'a usage::Counter,
    /// A connection to the store, to find an event recorded before.
    pub db: &'a Mutex<Connection>,
}

/// What became of one crash file.
enum Fate {
    Recorded,
    /// The store has its event from an earlier scan whose delete failed,
    /// so it is not counted in usage again.
    Known,
    /// The store did not take it, so the file waits for the next scan.
    Wait(Refused),
    /// The file cannot be read or parsed, so it is set aside.
    Refused(String),
}

/// Records up to [`PER_SCAN`] of `files`, the oldest first. Deletes each one the
/// store has, sets aside each one that cannot be read, and stops at the
/// first the store does not take. Runs outside the async runtime, as it
/// waits for the store writer. Returns the files recorded.
pub fn record(files: &[PathBuf], to: &Target<'_>, told: &mut Told) -> usize {
    let mut recorded = 0;
    for path in files.iter().take(PER_SCAN) {
        match record_one(path, to) {
            Fate::Recorded => {
                recorded += 1;
                delete(path, told);
            }
            Fate::Known => delete(path, told),
            Fate::Wait(why) => {
                if told.first(path) {
                    stderr(
                        COMPONENT,
                        format_args!("{} waits for the next try: {why}", path.display()),
                    );
                }
                break;
            }
            Fate::Refused(why) => set_aside(path, &why, told),
        }
    }
    recorded
}

fn record_one(path: &Path, to: &Target<'_>) -> Fate {
    let report = match read(path) {
        Ok(report) => report,
        Err(e) => return Fate::Refused(format!("{e:#}")),
    };
    if known(to.db, to.project, &report.event_id) {
        return Fate::Known;
    }
    let mut value = event(&report);
    let size = value.to_string().len();
    to.policy
        .item(&mut value, Item::Event, &mut Meter::unmetered());
    let event = match ErrorEvent::parse(value) {
        Ok(event) => event,
        Err(e) => return Fate::Refused(format!("{e:#}")),
    };
    let job = Job::Ingest {
        project: to.project,
        events: vec![event],
        held: to.budget.hold(),
    };
    match store::queue(to.sender, job).and_then(store::Queued::applied_blocking) {
        Ok(()) => {
            to.usage
                .add(to.project, Signal::Errors, Count::new(1, size));
            Fate::Recorded
        }
        Err(why) => Fate::Wait(why),
    }
}

/// Whether the store has the event `event_id` of `project`. A store that
/// cannot be read says no, and the ingest finds the event in its place.
fn known(db: &Mutex<Connection>, project: i64, event_id: &str) -> bool {
    let c = db.lock().unwrap_or_else(PoisonError::into_inner);
    c.query_row(
        "SELECT 1 FROM events WHERE project_id=?1 AND event_id=?2",
        params![project, event_id],
        |_| Ok(()),
    )
    .optional()
    .is_ok_and(|found| found.is_some())
}

fn delete(path: &Path, told: &mut Told) {
    if let Err(e) = fs::remove_file(path)
        && told.first(path)
    {
        stderr(
            COMPONENT,
            format_args!(
                "{} is recorded but was not deleted, and is not counted again: {e}",
                path.display()
            ),
        );
    }
}

/// Moves a file that cannot be recorded into [`REFUSED`], where it waits
/// for a person, or deletes it when [`MAX_PENDING`] wait there already.
fn set_aside(path: &Path, why: &str, told: &mut Told) {
    let Some(dir) = path.parent().map(|d| d.join(REFUSED)) else {
        return;
    };
    let full = pending(&dir).is_ok_and(|files| files.len() >= MAX_PENDING);
    let moved = if full {
        fs::remove_file(path)
            .map(|()| format!("deleted, as {MAX_PENDING} files are set aside already"))
    } else {
        super::make_dir(&dir)
            .and_then(|()| fs::rename(path, dir.join(path.file_name().unwrap_or_default())))
            .map(|()| format!("set aside in {}", dir.display()))
    };
    match moved {
        Ok(done) => stderr(
            COMPONENT,
            format_args!(
                "{} cannot be recorded, so it is {done}: {why}",
                path.display()
            ),
        ),
        Err(e) if told.first(path) => stderr(
            COMPONENT,
            format_args!(
                "{} cannot be recorded ({why}), and was not set aside: {e}",
                path.display()
            ),
        ),
        Err(_) => {}
    }
}

#[cfg(test)]
#[path = "ingest_tests.rs"]
mod tests;
