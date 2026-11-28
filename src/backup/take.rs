//! `davy backup`: the database as of the pause, copied with SQLite's backup,
//! the log directory and the crash reports as of the pause, linked, and the
//! other files of the data directory as they are, all written as one
//! archive.

use super::{
    RECORD,
    archive::{self, Header, Writer},
    link::{self, Linked},
    pause::{self, Paused},
};
use crate::self_report::crash;
use crate::store::{self, DB_FILE};
use anyhow::{Context as _, Result, bail, ensure};
use rusqlite::{Connection, OpenFlags, backup::StepResult};
use std::{
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// What a backup took.
#[derive(Debug)]
pub struct Taken {
    pub header: Header,
    /// The moment the backup holds: when its snapshot of the database was
    /// opened, during the pause.
    pub at_ms: i64,
    pub files: usize,
    /// The bytes of the files, before compression.
    pub bytes: u64,
    /// How long the log writer was paused.
    pub pause: Duration,
    /// Entries left out because they are neither files nor directories,
    /// such as symbolic links.
    pub skipped: Vec<String>,
    /// Files left out because their name is longer than a backup holds,
    /// see [`archive::MAX_NAME`]. Davy writes none.
    pub long: Vec<String>,
}

/// Files of the data directory a backup leaves out besides those starting
/// with a dot: SQLite's own beside the database, which the copy replaces,
/// the record of the last backup, and the `lost+found` of a data directory
/// that is a volume, which only root reads.
pub const LEFT_OUT: [&str; 7] = [
    "davy.sqlite",
    "davy.sqlite-wal",
    "davy.sqlite-shm",
    "davy.sqlite-journal",
    "davy.sqlite.lock",
    RECORD,
    archive::LOST_FOUND,
];

/// Where a backup stands, for tests that act at a given point.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Step {
    /// The log writer is paused; the snapshot is not open yet.
    Paused,
    /// The snapshot is open and the log writer paused; the logs are not
    /// linked yet.
    InPause,
    /// The pause is over; the snapshot is not copied yet.
    AfterPause,
}

/// Writes a backup of the data directory `data` to `out`, whether a server
/// runs on it or not. On an error `out` holds a backup without its
/// manifest, which verify and restore refuse.
pub fn take<W: Write>(data: &Path, out: W) -> Result<(W, Taken)> {
    take_with(data, out, &mut |_| ())
}

/// As [`take`], calling `at` at each [`Step`].
pub(super) fn take_with<W: Write>(
    data: &Path,
    out: W,
    at: &mut dyn FnMut(Step),
) -> Result<(W, Taken)> {
    super::owned(data, "backup")?;
    store::exists(data)?;
    let root = crate::logs::dir(data);
    link::mkdir_all(&root)?;
    let run = Run::new(&root)?;
    let moment = paused(data, &root, &run.dir, at)?;
    at(Step::AfterPause);
    let copy = run.dir.join(DB_FILE);
    moment.snapshot.copy(&copy)?;
    let schema = moment.snapshot.schema;
    drop(moment.snapshot);
    let mut list = List {
        files: vec![Listed {
            rel: PathBuf::from(DB_FILE),
            path: copy,
            staged: true,
        }],
        skipped: moment.linked.skipped,
        long: Vec::new(),
    };
    list.walk(data, Path::new(""), false, true)?;
    for staged in [crate::logs::DIR, crash::DIR] {
        list.walk(&run.dir.join(staged), Path::new(staged), true, false)?;
    }
    let (files, bytes) = list.sizes()?;
    // The copy of the database is staged too. A staged file missing at
    // write time fails the backup, so what it writes is what it linked.
    let staged = files.iter().filter(|f| f.staged).count() as u64 - 1;
    ensure!(
        staged == moment.linked.files,
        "this backup linked {} files into {} and found {staged} there: was its staging directory removed?",
        moment.linked.files,
        run.dir.display()
    );
    let header = Header::now(schema, moment.at_ms, files.len() as u64, bytes);
    let mut w = Writer::new(out, &header)?;
    for f in &files {
        add(&mut w, f)?;
    }
    let (out, manifest) = w.finish()?;
    Ok((
        out,
        Taken {
            header,
            at_ms: moment.at_ms,
            files: manifest.files.len(),
            bytes: manifest.bytes,
            pause: moment.pause,
            skipped: list.skipped,
            long: list.long,
        },
    ))
}

/// What the pause took.
struct Moment {
    snapshot: Snapshot,
    linked: Linked,
    pause: Duration,
    at_ms: i64,
}

/// The pause: the database's snapshot, and the links of the crash reports
/// and of the log directory, taken while the log writer writes nothing.
/// The crash reports go first: one recorded and deleted after their links
/// is in the snapshot. See docs/design.md.
fn paused(data: &Path, root: &Path, run: &Path, at: &mut dyn FnMut(Step)) -> Result<Moment> {
    let pause = Paused::take(root)?;
    if !pause.writer() && store::in_use(data)? {
        bail!(
            "another process has {} open, and no log writer of this Davy runs on it. A Davy server older than {} cannot pause for a backup: stop it and run davy backup again. A davy command holds the database while it runs: run davy backup again when it ends",
            data.join(DB_FILE).display(),
            env!("CARGO_PKG_VERSION")
        );
    }
    let start = Instant::now();
    at(Step::Paused);
    let mut linked =
        link::all(&crash::dir(data), &run.join(crash::DIR)).context("linking the crash reports")?;
    let at_ms = crate::time::now_ms();
    let snapshot = Snapshot::open(data)?;
    at(Step::InPause);
    let logs = link::logs(root, &run.join(crate::logs::DIR)).context("linking the logs")?;
    linked.files += logs.files;
    linked.skipped.extend(logs.skipped);
    let pause_took = start.elapsed();
    pause.end()?;
    Ok(Moment {
        snapshot,
        linked,
        pause: pause_took,
        at_ms,
    })
}

/// A read transaction on the database, opened during the pause. SQLite's
/// backup copies from it, so the copy is the database as of the pause,
/// whatever the server writes after.
struct Snapshot {
    c: Connection,
    schema: u32,
}

impl Snapshot {
    fn open(data: &Path) -> Result<Self> {
        let c = Connection::open_with_flags(
            data.join(DB_FILE),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        c.busy_timeout(store::BUSY_TIMEOUT)?;
        c.execute_batch("BEGIN")?;
        // The transaction reads at its first statement, not at BEGIN.
        let schema: u32 = c.query_row("SELECT user_version FROM pragma_user_version", [], |r| {
            r.get(0)
        })?;
        c.query_row("SELECT count(*) FROM sqlite_schema", [], |r| {
            r.get::<_, i64>(0)
        })?;
        Ok(Self { c, schema })
    }

    /// Copies the snapshot into a new database at `to`, in one step, so
    /// the backup reads every page inside this transaction.
    fn copy(&self, to: &Path) -> Result<()> {
        let mut dst = Connection::open(to)?;
        {
            let b = rusqlite::backup::Backup::new(&self.c, &mut dst)?;
            match b.step(-1).context("copying the database")? {
                StepResult::Done => {}
                other => bail!("copying the database stopped: {other:?}"),
            }
        }
        dst.close().map_err(|(_, e)| e)?;
        Ok(())
    }
}

/// A file to back up.
struct Listed {
    /// Its path relative to the data directory.
    rel: PathBuf,
    path: PathBuf,
    /// Whether it is in the staging directory, where nothing but this
    /// backup deletes a file.
    staged: bool,
}

/// The files a backup holds, listed before it writes any, so the header
/// can give their number and size.
struct List {
    files: Vec<Listed>,
    skipped: Vec<String>,
    long: Vec<String>,
}

impl List {
    /// Adds the files under `dir`, `rel` being its path relative to the
    /// data directory. At the top of the data directory it leaves out the
    /// log directory and the crash reports, linked during the pause, and
    /// [`LEFT_OUT`].
    fn walk(&mut self, dir: &Path, rel: &Path, staged: bool, top: bool) -> Result<()> {
        if staged && !dir.exists() && dir.parent().is_some_and(Path::is_dir) {
            // No crash report waited, or the directory was removed, which
            // the count of what was linked tells.
            return Ok(());
        }
        for (name, path, kind) in link::entries(dir)? {
            let skip = [crate::logs::DIR, crash::DIR].contains(&name.as_str())
                || LEFT_OUT.contains(&name.as_str());
            if top && skip {
                continue;
            }
            let rel = rel.join(&name);
            if kind.is_dir() {
                self.walk(&path, &rel, staged, false)?;
            } else if kind.is_file() && !staged && too_long(&rel) {
                self.long.push(rel.display().to_string());
            } else if kind.is_file() {
                self.files.push(Listed { rel, path, staged });
            } else {
                self.skipped.push(rel.display().to_string());
            }
        }
        Ok(())
    }

    /// The files and the sum of their sizes. A file of the data directory
    /// deleted since it was listed is left out, as a crash after the
    /// deletion would leave it; a staged one missing fails the backup.
    fn sizes(&mut self) -> Result<(Vec<Listed>, u64)> {
        let mut sized = Vec::with_capacity(self.files.len());
        let mut bytes = 0_u64;
        for f in self.files.drain(..) {
            match fs::metadata(&f.path) {
                Ok(m) => {
                    bytes += m.len();
                    sized.push(f);
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound && !f.staged => {}
                Err(e) => return Err(missing(e, &f)),
            }
        }
        Ok((sized, bytes))
    }
}

/// Whether `rel`, a path relative to the data directory, is longer in a
/// backup than [`archive::MAX_NAME`].
fn too_long(rel: &Path) -> bool {
    archive::DATA.len() + 1 + rel.as_os_str().len() > archive::MAX_NAME
}

/// Adds the listed file `f`. One that grew since it was listed fails the
/// backup when the files pass the sum the header gives.
fn add<W: Write>(w: &mut Writer<W>, f: &Listed) -> Result<()> {
    let file = match File::open(&f.path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound && !f.staged => return Ok(()),
        Err(e) => return Err(missing(e, f)),
    };
    let size = file.metadata()?.len();
    w.add(&f.rel, size, io::BufReader::with_capacity(1 << 16, file))
}

fn missing(e: io::Error, f: &Listed) -> anyhow::Error {
    let e = anyhow::Error::from(e);
    if f.staged {
        e.context(format!(
            "reading {}, which this backup staged: was its staging directory removed?",
            f.path.display()
        ))
    } else {
        e.context(format!("reading {}", f.path.display()))
    }
}

/// A backup's staging directory in the gate, which holds the links and
/// the copy of the database. Its lock tells a running backup's from one a
/// killed backup left, which [`pause::sweep`] removes. It is made as
/// [`pause::NEW`], which a sweep leaves while it is young, locked, then
/// renamed, so a sweep never removes the directory of a running backup.
struct Run {
    dir: PathBuf,
    _lock: File,
}

impl Run {
    fn new(root: &Path) -> Result<Self> {
        let gate = root.join(pause::GATE);
        link::mkdir_all(&gate)?;
        pause::sweep(&gate);
        let id = format!("{}-{}", std::process::id(), crate::time::now_ns());
        let new = gate.join(format!("{}{id}", pause::NEW));
        let dir = gate.join(format!("{}{id}", pause::RUN));
        link::mkdir(&new)?;
        let made = File::open(&new)
            .and_then(|lock| lock.lock().map(|()| lock))
            .and_then(|lock| fs::rename(&new, &dir).map(|()| lock));
        match made {
            Ok(lock) => Ok(Self { dir, _lock: lock }),
            Err(e) => {
                drop(fs::remove_dir(&new));
                Err(e).with_context(|| format!("making {}", dir.display()))
            }
        }
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        drop(fs::remove_dir_all(&self.dir));
    }
}

#[cfg(test)]
#[path = "take_tests.rs"]
mod tests;
