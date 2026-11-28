//! How a backup pauses the server's log writer, which runs in another
//! process: with `flock` on three paths in the log directory, and a count
//! in a fourth.
//!
//! - The log directory itself: the writer holds it shared whenever it waits
//!   for backups, and a backup takes it exclusive for its pause.
//! - [`GATE`]: a backup holds it exclusive from before it asks until after
//!   its pause. The writer tries it shared at the end of each turn of its
//!   loop, and when it is taken it lets go of the log directory and waits.
//! - [`RUNNING`] in it: the writer holds it shared as long as it runs, so a
//!   backup knows whether a writer that pauses exists.
//! - [`LOOSE`] in it: a count the writer adds one to when it stops waiting
//!   for a backup, after [`WRITER_WAIT`], and again when it holds the log
//!   directory once more. An odd count means it writes without the log
//!   directory. A backup reads the count when it takes the gate and again
//!   at the end of its pause, and fails when the two differ. No lock waits
//!   on it, so a backup stopped anywhere holds the writer back
//!   [`WRITER_WAIT`] at most, unless the writer cannot write the count, on
//!   a full disk or a read-only one where it could not write logs either:
//!   then it waits until it can or the backup ends, and reports an error
//!   every [`WRITER_WAIT`].
//!
//! See "Backup and restore" in docs/design.md.

use anyhow::{Context as _, Result, bail};
use std::{
    fs::{File, OpenOptions, TryLockError},
    io::{self, Write as _},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

/// In the log directory: the gate, and where a backup puts its links.
pub const GATE: &str = ".backup";
/// The start of the name of a backup's staging directory in the gate.
pub const RUN: &str = "run-";
/// The start of the name of a staging directory not yet locked. A sweep
/// leaves one younger than [`BACKUP_WAIT`]: its backup locks it within
/// microseconds of making it.
pub const NEW: &str = "new-";
const RUNNING: &str = "running";
const LOOSE: &str = "loose";
/// How long the log writer waits for a backup's pause to end before it
/// writes again, until that backup ends. A pause took 0.4 to 2.5 s on a
/// store of 2.9 GB, see docs/design.md.
pub const WRITER_WAIT: Duration = Duration::from_secs(30);
/// How long a backup waits for another one to finish, then for a writer
/// that stopped waiting to hold the log directory again, and then for it
/// to pause: at the end of a turn, which can hold a commit and a retention
/// pass.
pub const BACKUP_WAIT: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(10);

/// The log writer's side.
pub struct Gate {
    logs: File,
    /// [`GATE`], which a backup holds while it asks for a pause.
    asking: File,
    /// Held for the life of the writer, never read.
    _running: File,
    count: PathBuf,
    /// Whether this holds the log directory shared.
    holding: bool,
    /// Whether this stopped waiting for a backup that still runs, and
    /// writes without the log directory.
    loose: bool,
    /// [`WRITER_WAIT`], shorter in tests.
    patience: Duration,
}

impl Gate {
    /// Opens the locks in the log directory `root`. Call [`Gate::check`]
    /// next, before the writer touches a file: it waits for a backup's
    /// pause to end.
    pub fn open(root: &Path) -> Result<Self> {
        let files = Files::open(root)?;
        sweep(&root.join(GATE));
        files
            .running
            .lock_shared()
            .context("locking the backup gate")?;
        let count = root.join(GATE).join(LOOSE);
        // An odd count is a writer that died loose: this one starts even.
        if loose_count(&count)? % 2 == 1 {
            bump(&count)?;
        }
        Ok(Self {
            logs: files.logs,
            asking: files.gate,
            _running: files.running,
            count,
            holding: false,
            loose: false,
            patience: WRITER_WAIT,
        })
    }

    /// Pauses while a backup asks for it. Call it where a crash would
    /// leave the files as recovery takes them: between turns of the loop.
    /// Ok means it holds the log directory, or stopped waiting for the
    /// backup that still asks. Fails when it waited [`WRITER_WAIT`] and
    /// writes again: the next checks return at once until that backup ends.
    /// It waits on while it cannot write [`LOOSE`].
    pub fn check(&mut self) -> Result<()> {
        if self.pass()? || self.loose {
            return Ok(());
        }
        if self.holding {
            self.logs.unlock()?;
            self.holding = false;
        }
        self.wait()
    }

    /// Whether no backup asks and this holds the log directory.
    fn pass(&mut self) -> Result<bool> {
        if !try_lock(self.asking.try_lock_shared())? {
            return Ok(false);
        }
        let got = if self.holding {
            Ok(true)
        } else {
            try_lock(self.logs.try_lock_shared())
        };
        self.asking.unlock()?;
        if !got? {
            return Ok(false);
        }
        self.holding = true;
        if self.loose {
            bump(&self.count)?;
            self.loose = false;
        }
        Ok(true)
    }

    fn wait(&mut self) -> Result<()> {
        let start = Instant::now();
        loop {
            if self.pass()? {
                return Ok(());
            }
            if start.elapsed() >= self.patience {
                break;
            }
            thread::sleep(POLL);
        }
        // Without the count the backup could not tell, so the writer waits
        // on rather than let it write a bad file.
        while let Err(e) = bump(&self.count) {
            if self.pass()? {
                return Ok(());
            }
            crate::self_report::log(
                crate::logs::LogLevel::Error,
                "log writer",
                format_args!("backup pause: cannot stop waiting: {e:#}"),
            );
            thread::sleep(self.patience);
        }
        self.loose = true;
        bail!(
            "a backup held the log writer for {} ms: it writes on until that backup ends, and that backup fails",
            self.patience.as_millis()
        )
    }
}

/// A backup's pause: the log writer of this directory, if one runs,
/// writes nothing until [`Paused::end`] or the drop.
pub struct Paused {
    gate: File,
    logs: File,
    count: PathBuf,
    /// [`LOOSE`] when the gate was taken.
    at: u64,
    writer: bool,
}

impl Paused {
    /// Asks the log writer in `root` to pause and waits until it has, at
    /// most [`BACKUP_WAIT`] for another backup, as long for a writer that
    /// stopped waiting for an earlier one, and as long again for the
    /// writer to pause. Without a writer it returns at once.
    pub fn take(root: &Path) -> Result<Self> {
        let files = Files::open(root)?;
        let count = root.join(GATE).join(LOOSE);
        let start = Instant::now();
        let at = loop {
            wait_for(
                || files.gate.try_lock(),
                "another backup of this directory still runs",
            )?;
            let at = loose_count(&count)?;
            if at % 2 == 0 || !running(root)? {
                break at;
            }
            // A writer still loose from an earlier backup holds the log
            // directory again at its next check, once the gate is free.
            files.gate.unlock()?;
            if start.elapsed() >= BACKUP_WAIT {
                bail!(
                    "the log writer still writes without waiting for backups, after an earlier one held it too long (waited {} s): try again",
                    BACKUP_WAIT.as_secs()
                );
            }
            while loose_count(&count)? == at && start.elapsed() < BACKUP_WAIT {
                thread::sleep(POLL);
            }
        };
        wait_for(
            || files.logs.try_lock(),
            "the log writer did not pause: it may be stuck in a long commit, try again",
        )?;
        let writer = !try_lock(files.running.try_lock())?;
        Ok(Self {
            gate: files.gate,
            logs: files.logs,
            count,
            at,
            writer,
        })
    }

    /// Whether a log writer of a Davy that pauses runs. Without one, an
    /// older server could still be writing.
    pub fn writer(&self) -> bool {
        self.writer
    }

    /// Ends the pause. Fails when the writer stopped waiting during it,
    /// since files may have changed while they were linked.
    pub fn end(self) -> Result<()> {
        if loose_count(&self.count)? != self.at {
            bail!(
                "the log writer stopped waiting and wrote during the backup's pause: take the backup again"
            );
        }
        self.logs.unlock()?;
        self.gate.unlock()?;
        Ok(())
    }
}

struct Files {
    logs: File,
    gate: File,
    running: File,
}

impl Files {
    fn open(root: &Path) -> Result<Self> {
        let gate = root.join(GATE);
        super::link::mkdir_all(&gate)?;
        let open = |p: &Path| File::open(p).with_context(|| format!("opening {}", p.display()));
        Ok(Self {
            logs: open(root)?,
            gate: open(&gate)?,
            running: lock_file(&gate.join(RUNNING))?,
        })
    }
}

/// [`LOOSE`]: none is 0.
fn loose_count(path: &Path) -> Result<u64> {
    match std::fs::read_to_string(path) {
        Ok(s) if s.trim().is_empty() => Ok(0),
        Ok(s) => s
            .trim()
            .parse()
            .with_context(|| format!("reading {}: not a count", path.display())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Adds one to [`LOOSE`], replacing it whole so a backup never reads half.
fn bump(path: &Path) -> Result<()> {
    let next = loose_count(path)? + 1;
    let tmp = path.with_extension(format!(
        "tmp-{}-{}",
        std::process::id(),
        crate::time::now_ns()
    ));
    let mut o = OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    let written = o
        .open(&tmp)
        .and_then(|mut f| f.write_all(next.to_string().as_bytes()))
        .and_then(|()| std::fs::rename(&tmp, path));
    if let Err(e) = written {
        drop(std::fs::remove_file(&tmp));
        return Err(e).with_context(|| format!("writing {}", path.display()));
    }
    Ok(())
}

/// Whether a log writer of this Davy runs on the log directory `root`.
pub fn running(root: &Path) -> Result<bool> {
    let path = root.join(GATE).join(RUNNING);
    match File::open(&path) {
        Ok(f) => Ok(!try_lock(f.try_lock())?),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e).with_context(|| format!("opening {}", path.display())),
    }
}

/// Removes what killed backups and writers left in `gate`: staging
/// directories whose lock no process holds, which hold links to log files
/// and a copy of the database, and half-written counts. A [`NEW`] one goes
/// only once older than [`BACKUP_WAIT`], as its backup has not locked it
/// yet. Errors are ignored: the next sweep tries again.
pub fn sweep(gate: &Path) {
    let Ok(entries) = std::fs::read_dir(gate) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(&format!("{LOOSE}.tmp-")) {
            drop(std::fs::remove_file(&path));
            continue;
        }
        let swept = name.starts_with(RUN) || (name.starts_with(NEW) && old(&entry));
        if !swept {
            continue;
        }
        if let Ok(dir) = File::open(&path)
            && dir.try_lock().is_ok()
        {
            drop(std::fs::remove_dir_all(&path));
        }
    }
}

/// Whether `entry` was last changed [`BACKUP_WAIT`] ago or longer. One
/// whose time cannot be read, or lies ahead, is young.
fn old(entry: &std::fs::DirEntry) -> bool {
    entry
        .metadata()
        .and_then(|m| m.modified())
        .is_ok_and(|at| at.elapsed().is_ok_and(|age| age >= BACKUP_WAIT))
}

fn lock_file(path: &Path) -> Result<File> {
    let mut o = OpenOptions::new();
    o.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    o.open(path)
        .with_context(|| format!("opening {}", path.display()))
}

/// Whether a lock attempt got the lock.
pub(super) fn try_lock(r: Result<(), TryLockError>) -> Result<bool> {
    match r {
        Ok(()) => Ok(true),
        Err(TryLockError::WouldBlock) => Ok(false),
        Err(TryLockError::Error(e)) => Err(e.into()),
    }
}

fn wait_for(mut attempt: impl FnMut() -> Result<(), TryLockError>, why: &str) -> Result<()> {
    let start = Instant::now();
    while !try_lock(attempt())? {
        if start.elapsed() >= BACKUP_WAIT {
            bail!("{why} (waited {} s)", BACKUP_WAIT.as_secs());
        }
        thread::sleep(POLL);
    }
    Ok(())
}

#[cfg(test)]
#[path = "pause_tests.rs"]
mod tests;
