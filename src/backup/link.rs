//! The log directory hard-linked into a backup's staging directory during
//! the pause: cheap, and a link keeps the bytes of a file that a merge or
//! retention deletes afterwards. Segment files never change once written,
//! so a link is as good as a copy.

use anyhow::{Context as _, Result, bail};
use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
};

/// A day index's list of segments, which Tantivy replaces whole.
const META: &str = "meta.json";
/// A background merge can replace `meta.json` while a day is linked; each
/// change starts that day again.
const ATTEMPTS: u32 = 20;

/// What [`logs`] linked, and what it left out.
#[derive(Debug, Default, PartialEq)]
pub struct Linked {
    pub files: u64,
    /// Symbolic links and other entries that are neither a file nor a
    /// directory, relative to the log directory.
    pub skipped: Vec<String>,
}

/// Links the log directory `root` into `into`. Entries whose name starts
/// with a dot are left out at every level: the gate and the staging
/// directories, `.trash`, a cold segment being written (`.N.tmp`), and
/// Tantivy's lock files, all of which recovery removes or ignores.
pub fn logs(root: &Path, into: &Path) -> Result<Linked> {
    let mut linked = Linked::default();
    mkdir(into)?;
    for (name, path, kind) in entries(root)? {
        let to = into.join(&name);
        if kind.is_dir() && path.join(META).exists() {
            day(&path, &to, &mut linked, &mut || ())?;
        } else if kind.is_dir() {
            tree(&path, &to, Path::new(&name), &mut linked)?;
        } else if kind.is_file() {
            link(&path, &to, &mut linked)?;
        } else {
            linked.skipped.push(name);
        }
    }
    Ok(linked)
}

/// Links a day index. Only the segment files `meta.json` names, so files a
/// merge is still writing stay out, and `meta.json` read before the links
/// and still the same after them, so every segment it names was linked.
/// `linked_once` runs after each round of links, for tests.
fn day(dir: &Path, into: &Path, linked: &mut Linked, linked_once: &mut dyn FnMut()) -> Result<()> {
    for _ in 0..ATTEMPTS {
        let before = fs::read(dir.join(META)).with_context(|| read_err(dir, META))?;
        let segments = segments(&before).with_context(|| read_err(dir, META))?;
        if into.exists() {
            fs::remove_dir_all(into)?;
        }
        mkdir(into)?;
        let mut day = Linked::default();
        for (name, path, kind) in entries(dir)? {
            if name == META || segment_of(&name).is_some_and(|s| !segments.contains(s)) {
                continue;
            }
            if kind.is_file() {
                link(&path, &into.join(&name), &mut day)?;
            } else {
                day.skipped.push(format!("{}/{name}", dir.display()));
            }
        }
        linked_once();
        if optional(&dir.join(META))?.as_deref() == Some(before.as_slice()) {
            fs::write(into.join(META), &before)?;
            linked.files += day.files + 1;
            linked.skipped.extend(day.skipped);
            return Ok(());
        }
    }
    bail!(
        "{} kept changing while it was linked, {ATTEMPTS} times",
        dir.join(META).display()
    )
}

/// The segment ids in a `meta.json`, as Tantivy names their files: 32
/// lowercase hex digits.
fn segments(meta: &[u8]) -> Result<HashSet<String>> {
    let meta: serde_json::Value = serde_json::from_slice(meta)?;
    let list = meta["segments"].as_array().context("no list of segments")?;
    list.iter()
        .map(|s| {
            s["segment_id"]
                .as_str()
                .map(|id| id.replace('-', "").to_lowercase())
                .context("a segment without an id")
        })
        .collect()
}

/// The segment a file of a day index belongs to, by its name.
fn segment_of(name: &str) -> Option<&str> {
    let stem = name.split('.').next()?;
    (stem.len() == 32 && stem.bytes().all(|b| b.is_ascii_hexdigit())).then_some(stem)
}

/// Links every file under `dir`, the cold tier or anything else that is not
/// a day index.
fn tree(dir: &Path, into: &Path, shown: &Path, linked: &mut Linked) -> Result<()> {
    mkdir(into)?;
    for (name, path, kind) in entries(dir)? {
        if kind.is_dir() {
            tree(&path, &into.join(&name), &shown.join(&name), linked)?;
        } else if kind.is_file() {
            link(&path, &into.join(&name), linked)?;
        } else {
            linked.skipped.push(shown.join(&name).display().to_string());
        }
    }
    Ok(())
}

/// Links every file under `dir` into `into`, which it makes: the crash
/// reports waiting to be recorded. None when `dir` does not exist.
pub fn all(dir: &Path, into: &Path) -> Result<Linked> {
    let mut linked = Linked::default();
    if fs::symlink_metadata(dir).is_ok_and(|m| m.is_dir()) {
        tree(dir, into, Path::new(""), &mut linked)?;
    }
    Ok(linked)
}

/// Links `from` to `to`. A file deleted since its directory was read is
/// left out, as a crash after that deletion would leave it; any other
/// failure, such as a staging directory gone, fails the backup.
fn link(from: &Path, to: &Path, linked: &mut Linked) -> Result<()> {
    match fs::hard_link(from, to) {
        Ok(()) => {
            linked.files += 1;
            Ok(())
        }
        Err(e)
            if e.kind() == io::ErrorKind::NotFound
                && fs::symlink_metadata(from)
                    .is_err_and(|m| m.kind() == io::ErrorKind::NotFound)
                && to.parent().is_some_and(Path::is_dir) =>
        {
            Ok(())
        }
        Err(e) => Err(e).with_context(|| format!("linking {} to {}", from.display(), to.display())),
    }
}

/// The entries of `dir` whose name does not start with a dot, sorted. A
/// name that is not UTF-8 is an error: Davy writes none.
pub fn entries(dir: &Path) -> Result<Vec<(String, PathBuf, fs::FileType)>> {
    let mut found = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|n| anyhow::anyhow!("{} is not UTF-8", dir.join(n).display()))?;
        if name.starts_with('.') {
            continue;
        }
        let kind = match entry.file_type() {
            Ok(kind) => kind,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        found.push((name, entry.path(), kind));
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(found)
}

fn optional(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

fn read_err(dir: &Path, name: &str) -> String {
    format!("reading {}", dir.join(name).display())
}

/// Makes `dir` for the owner only. Its parent must exist: a staging
/// directory removed under a backup stays removed, and the backup fails.
pub fn mkdir(dir: &Path) -> Result<()> {
    make(dir, false)
}

/// Makes `dir` for the owner only, and its parents.
pub fn mkdir_all(dir: &Path) -> Result<()> {
    make(dir, true)
}

fn make(dir: &Path, parents: bool) -> Result<()> {
    let mut b = fs::DirBuilder::new();
    b.recursive(parents);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
    b.create(dir)
        .with_context(|| format!("creating {}", dir.display()))
}

#[cfg(test)]
#[path = "link_tests.rs"]
mod tests;
