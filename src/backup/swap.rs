//! How a restore puts a checked backup in place of what the data directory
//! held, so that a crash anywhere leaves a state the next `davy restore` or
//! `davy serve` finishes. The state is in names inside the data directory:
//!
//! 1. `.restore-PID-NS`: the backup being extracted and checked. Nothing
//!    else changed; a crash leaves only this, which goes at the next start.
//! 2. [`READY`]: the same directory renamed once it is whole and on disk,
//!    holding [`ASIDE`], the name of the directory that takes what the data
//!    directory held. This rename is the commit point: from here on the
//!    restore goes forward. What the data directory holds besides the
//!    restore's own names is moved there.
//! 3. [`MOVING`]: renamed again once nothing old is left. Its entries move
//!    into the data directory one by one, the database last, then
//!    [`ASIDE`] goes, then [`MOVING`]. One without [`ASIDE`] is done.
//!
//! Finishing never overwrites a file: a name in the way stops it with a
//! message, and once that name is moved away the next try goes on.
//!
//! A server holds [`Claim`] shared for its life and a restore holds it
//! exclusive, so neither starts while the other runs. A davy command that
//! opens the database refuses while a restore runs or one stopped halfway,
//! see [`idle`]. See "Restore" in docs/design.md.

use super::link;
use crate::store::DB_FILE;
use anyhow::{Context as _, Result, bail, ensure};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write as _},
    path::{Path, PathBuf},
};

/// The start of every name a restore makes in the data directory.
pub const STAGING: &str = ".restore-";
/// The checked backup, before anything old moved.
const READY: &str = ".restore-ready";
/// The checked backup, after everything old moved.
const MOVING: &str = ".restore-moving";
/// What [`Claim`] locks: a directory of its own, not the data directory,
/// which the retention lock takes exclusive while a server runs.
pub const CLAIM: &str = ".restore-claim";
/// In [`READY`]: the name of the directory that takes what was there.
pub const ASIDE: &str = ".aside";
/// Where a restore moves what the data directory held.
pub const REPLACED: &str = ".replaced-";
/// SQLite's files beside the database. One of them beside a restored
/// database would be taken as its own.
const BESIDE: [&str; 3] = ["davy.sqlite-wal", "davy.sqlite-shm", "davy.sqlite-journal"];

/// The data directory's claim, a lock on [`CLAIM`] in it.
pub struct Claim {
    _dir: File,
    data: PathBuf,
}

impl Claim {
    /// For `davy serve`, held until it exits: shared, so a restore cannot
    /// start under it. Finishes a restore that stopped past its commit
    /// point first. Creates a missing data directory.
    pub fn serve(data: &Path) -> Result<Self> {
        link::mkdir_all(data)?;
        let dir = open(data)?;
        if super::pause::try_lock(dir.try_lock())? {
            if let Some(aside) = finish(data)? {
                crate::self_report::stderr(
                    "restore",
                    format!(
                        "finished the restore into {} that stopped halfway; {}",
                        data.display(),
                        replaced(&aside)
                    ),
                );
            }
            dir.lock_shared()?;
        } else {
            ensure!(
                super::pause::try_lock(dir.try_lock_shared())?,
                "a restore into {} runs: start the server when it ends",
                data.display()
            );
            // Another server, a backup or a davy command holds it shared,
            // and none of those runs on a restore stopped halfway.
            pending(data)?;
        }
        Ok(Self {
            _dir: dir,
            data: data.to_path_buf(),
        })
    }

    /// For `davy restore`: exclusive, refused while a server of this Davy
    /// runs.
    pub fn restore(data: &Path) -> Result<Self> {
        link::mkdir_all(data)?;
        let dir = open(data)?;
        if !super::pause::try_lock(dir.try_lock())? {
            bail!(
                "a Davy server or another restore runs on {}, or a backup of it: stop it before you restore into it",
                data.display()
            );
        }
        Ok(Self {
            _dir: dir,
            data: data.to_path_buf(),
        })
    }
}

/// For a davy command that opens the database in `data`: refuses while a
/// restore runs, and when one stopped halfway, whose next step could
/// otherwise move away or overwrite what the command wrote. Returns the
/// claim held shared, `None` when no restore or server ever ran on
/// `data`, which this does not create: a command run as root would leave
/// a claim the server cannot open.
pub fn idle(data: &Path) -> Result<Option<File>> {
    let path = data.join(CLAIM);
    let dir = match File::open(&path) {
        Ok(dir) => Some(dir),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e).with_context(|| format!("opening {}", path.display())),
    };
    if let Some(dir) = &dir {
        ensure!(
            super::pause::try_lock(dir.try_lock_shared())?,
            "a restore into {} runs: run this again when it ends",
            data.display()
        );
    }
    pending(data)?;
    Ok(dir)
}

/// Refuses a data directory with a restore stopped past its commit point.
fn pending(data: &Path) -> Result<()> {
    if data.join(READY).exists() || data.join(MOVING).exists() {
        let shown = data.display();
        bail!(
            "a restore into {shown} stopped halfway: start davy serve on it, or run davy restore --data {shown} -, which finishes it and reads nothing"
        );
    }
    Ok(())
}

impl Claim {
    /// The data directory claimed.
    pub fn data(&self) -> &Path {
        &self.data
    }
}

fn open(data: &Path) -> Result<File> {
    let path = data.join(CLAIM);
    link::mkdir_all(&path)?;
    let dir = File::open(&path).with_context(|| format!("opening {}", path.display()))?;
    ensure!(
        dir.metadata()?.is_dir(),
        "{} is not a directory",
        path.display()
    );
    Ok(dir)
}

/// Makes the checked backup in `staging` ready to commit: picks a name
/// for what the data directory holds that nothing has, writes it to
/// [`ASIDE`], and syncs it and every directory under `staging`, whose files
/// the extract synced, so the commit holds after a power cut.
pub fn seal(data: &Path, staging: &Path) -> Result<()> {
    let stamp = jiff::Timestamp::now().strftime("%Y%m%dT%H%M%SZ");
    let name = format!("{REPLACED}{stamp}-{}", std::process::id());
    let name = free(data, &name)?;
    write_synced(&staging.join(ASIDE), &name)?;
    sync_tree(staging)
}

/// `name`, or with a number after it when something in `data` has it.
fn free(data: &Path, name: &str) -> Result<String> {
    for n in 0..100 {
        let candidate = if n == 0 {
            name.to_owned()
        } else {
            format!("{name}-{n}")
        };
        match fs::symlink_metadata(data.join(&candidate)) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(candidate),
            Err(e) => return Err(e).with_context(|| format!("reading {}", data.display())),
            Ok(_) => {}
        }
    }
    bail!("{} holds {name} and 99 more like it", data.display())
}

/// Writes `text` to a new file at `path` and syncs it.
fn write_synced(path: &Path, text: &str) -> Result<()> {
    let mut o = OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, super::archive::FILE_MODE);
    o.open(path)
        .and_then(|mut f| f.write_all(text.as_bytes()).and_then(|()| f.sync_all()))
        .with_context(|| format!("writing {}", path.display()))
}

/// Commits the checked backup in `staging`, which [`seal`] made ready, and
/// puts it in place. Hold the [`Claim`] exclusive.
pub fn commit(data: &Path, staging: &Path) -> Result<PathBuf> {
    sync(data)?;
    fs::rename(staging, data.join(READY)).context("committing the restore")?;
    sync(data)?;
    finish(data)?.context("the committed restore went missing")
}

/// Finishes a restore into `data` that stopped past its commit point, and
/// removes what one stopped before it left. Returns where what the data
/// directory held went, when there was one to finish. Hold the [`Claim`]
/// exclusive.
pub fn finish(data: &Path) -> Result<Option<PathBuf>> {
    sweep(data);
    let ready = data.join(READY);
    let moving = data.join(MOVING);
    if ready.exists() {
        let aside = move_aside(data, &ready)?;
        // The moves out are on disk before the name says they are done.
        sync(data)?;
        fs::rename(&ready, &moving)?;
        sync(data)?;
        move_in(data, &moving)?;
        return Ok(Some(aside));
    }
    if !moving.exists() {
        return Ok(None);
    }
    if !moving.join(ASIDE).exists() {
        // Everything moved in, and only the empty directory is left.
        fs::remove_dir(&moving).with_context(|| format!("removing {}", moving.display()))?;
        sync(data)?;
        return Ok(None);
    }
    let aside = data.join(aside_name(&moving)?);
    move_in(data, &moving)?;
    Ok(Some(aside))
}

/// The name [`ASIDE`] in `journal` holds.
fn aside_name(journal: &Path) -> Result<String> {
    let path = journal.join(ASIDE);
    let name = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    ensure!(
        name.starts_with(REPLACED) && !name.contains(['/', '\\']),
        "{} names {name:?}, not a directory of the data directory",
        path.display()
    );
    Ok(name)
}

/// Moves what `data` holds into the directory [`ASIDE`] in `ready` names,
/// and returns that directory. Refuses before it moves anything when a
/// name is in both, as a process wrote it in `data` after a restore
/// stopped. When a file took the directory's name since the commit,
/// nothing was moved there yet, so it picks another.
fn move_aside(data: &Path, ready: &Path) -> Result<PathBuf> {
    let mut aside = data.join(aside_name(ready)?);
    let old = held(data)?;
    if old.is_empty() {
        return Ok(aside);
    }
    match fs::symlink_metadata(&aside) {
        Ok(m) if m.is_dir() => {}
        Ok(_) => {
            let name = free(
                data,
                &format!("{}-{}", aside_name(ready)?, crate::time::now_ns()),
            )?;
            write_synced(&ready.join(ASIDE), &name)?;
            aside = data.join(name);
            link::mkdir(&aside)?;
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => link::mkdir(&aside)?,
        Err(e) => return Err(e).with_context(|| format!("reading {}", aside.display())),
    }
    for name in &old {
        ensure!(
            fs::symlink_metadata(aside.join(name)).is_err(),
            "{name} is in {} and in {}: a process wrote it after the restore stopped. Move one of them away and run davy restore again to finish",
            data.display(),
            aside.display()
        );
    }
    for name in &old {
        fs::rename(data.join(name), aside.join(name))
            .with_context(|| format!("moving {name} aside"))?;
    }
    sync(&aside)?;
    Ok(aside)
}

/// Moves the entries of `moving` into `data`, the database last, and
/// removes `moving`. A name already in `data` stops it.
fn move_in(data: &Path, moving: &Path) -> Result<()> {
    let mut names: Vec<String> = link::entries(moving)?
        .into_iter()
        .map(|(name, ..)| name)
        .collect();
    names.sort_by_key(|n| n == DB_FILE);
    for name in names {
        if name == DB_FILE {
            for beside in BESIDE {
                ensure!(
                    !data.join(beside).exists(),
                    "{} appeared during the restore: a process opened the database. Stop it and run davy restore again to finish",
                    data.join(beside).display()
                );
            }
        }
        let to = data.join(&name);
        ensure!(
            fs::symlink_metadata(&to).is_err(),
            "{} appeared during the restore: a process wrote it. Move it away and run davy restore again to finish",
            to.display()
        );
        fs::rename(moving.join(&name), &to)
            .with_context(|| format!("moving {name} into {}", data.display()))?;
    }
    // The moves are on disk before the journal says they are done.
    sync(data)?;
    fs::remove_file(moving.join(ASIDE))?;
    fs::remove_dir(moving)?;
    sync(data)
}

/// What the data directory holds besides a restore's own names and what
/// earlier ones replaced. An ext4 volume mounted as the data directory has
/// lost+found, which stays.
pub fn held(data: &Path) -> Result<Vec<String>> {
    let mut had = Vec::new();
    for entry in fs::read_dir(data).with_context(|| format!("reading {}", data.display()))? {
        let name = entry?.file_name();
        let name = name
            .into_string()
            .map_err(|n| anyhow::anyhow!("{} is not UTF-8", data.join(n).display()))?;
        if !name.starts_with(REPLACED)
            && !name.starts_with(STAGING)
            && name != super::archive::LOST_FOUND
        {
            had.push(name);
        }
    }
    had.sort();
    Ok(had)
}

/// Removes the staging directories restores stopped before their commit
/// point left. Errors are ignored: the next start tries again.
fn sweep(data: &Path) {
    let Ok(entries) = fs::read_dir(data) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(STAGING) && ![READY, MOVING, CLAIM].contains(&name.as_str()) {
            drop(fs::remove_dir_all(entry.path()));
        }
    }
}

/// Where what a finished restore replaced went, in words.
pub fn replaced(aside: &Path) -> String {
    if aside.exists() {
        format!("what it replaced is in {}", aside.display())
    } else {
        "it replaced nothing".to_owned()
    }
}

/// Syncs the directory `dir`, so the renames in it are on disk.
pub fn sync(dir: &Path) -> Result<()> {
    File::open(dir)
        .and_then(|d| d.sync_all())
        .with_context(|| format!("syncing {}", dir.display()))
}

/// Syncs every directory under `dir`, and `dir`.
pub fn sync_tree(dir: &Path) -> Result<()> {
    for (_, path, kind) in link::entries(dir)? {
        if kind.is_dir() {
            sync_tree(&path)?;
        }
    }
    sync(dir)
}

#[cfg(test)]
#[path = "swap_tests.rs"]
mod tests;
