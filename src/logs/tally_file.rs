//! The files of the pattern counters: one per project and day in the day's
//! directory, so a flush rewrites only the projects that changed, and a
//! cut of projects removes their files without reading any.

use super::{Cut, EXAMPLE_BYTES, Entry, MAX_CELLS, MAX_PATTERNS, ProjectDay, SERVICE_BYTES};
use crate::time::now_ns;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs::File,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

const PREFIX: &str = "patterns-";
const SUFFIX: &str = ".json";
/// What follows a file's name in the name of its copy set aside.
const ASIDE: &str = ".unread-";
/// How the name of a temporary file ends, see [`write_durably`].
const TEMPORARY: &str = ".tmp";
const VERSION: u32 = 1;
/// Bytes a file may have. The bounds keep one project's day under about
/// 20 MB even when every byte of its text is escaped, so a bigger file is
/// not one Davy wrote.
pub(super) const MAX_FILE_BYTES: u64 = 64 << 20;
/// Bytes a template may have. The mask reads 4 KiB of a message and keeps
/// 12 tokens of 64 bytes, so its templates are far shorter.
const TEMPLATE_BYTES: usize = 4096;

#[derive(Serialize)]
struct Out<'a> {
    version: u32,
    patterns: &'a HashMap<u64, Entry>,
}

#[derive(Deserialize)]
struct In {
    version: u32,
    patterns: HashMap<u64, Entry>,
}

/// The name of the counters of `project` in a day's directory.
pub(in crate::logs::store) fn name(project: u64) -> String {
    format!("{PREFIX}{project}{SUFFIX}")
}

/// The project a file of the counters belongs to, its temporary copies
/// included, and whether it is the file itself.
fn project_of(file: &str) -> Option<(u64, bool)> {
    let rest = file.strip_prefix(PREFIX)?;
    let digits = rest.find(|c: char| !c.is_ascii_digit())?;
    let project: u64 = rest[..digits].parse().ok()?;
    rest[digits..].strip_prefix(SUFFIX)?;
    Some((project, file == name(project)))
}

/// The names in `dir`, none when it is gone.
fn names(dir: &Path) -> Result<Vec<String>> {
    match std::fs::read_dir(dir) {
        Ok(entries) => Ok(entries
            .filter_map(|e| Some(e.ok()?.file_name().to_str()?.to_owned()))
            .collect()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(anyhow::Error::from(e).context(format!("listing {}", dir.display()))),
    }
}

/// The projects with counters in `dir`, and those whose file was set
/// aside, see [`set_aside`].
pub(super) fn projects(dir: &Path) -> Result<(Vec<u64>, Vec<u64>)> {
    let (mut projects, mut aside) = (Vec::new(), Vec::new());
    for file in names(dir)? {
        match project_of(&file) {
            Some((project, true)) => projects.push(project),
            Some((project, false)) if file.starts_with(&format!("{}{ASIDE}", name(project))) => {
                aside.push(project);
            }
            _ => {}
        }
    }
    Ok((projects, aside))
}

/// Removes the temporary files a crash left in `dir`. Only the writer
/// writes a day's files, so none of them is being written.
pub(super) fn remove_temporary(dir: &Path) -> Result<()> {
    for file in names(dir)? {
        if project_of(&file).is_some_and(|(_, file)| !file) && file.ends_with(TEMPORARY) {
            let path = dir.join(&file);
            std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
        }
    }
    Ok(())
}

/// Whether `e`, an error of [`load`], says the file is not counters Davy
/// wrote, rather than that the disk or its rights kept it from being read.
pub(super) fn damaged(e: &anyhow::Error) -> bool {
    e.root_cause().downcast_ref::<io::Error>().is_none()
}

/// The counters of `project` in `dir`, if it has any. A file past the
/// bounds the writer keeps is an error, so a damaged or foreign file
/// cannot make the writer or a report hold more than they would.
pub(super) fn load(dir: &Path, project: u64) -> Result<Option<ProjectDay>> {
    let path = dir.join(name(project));
    let text = match read_capped(&path) {
        Ok(Some(text)) => text,
        Ok(None) => return Ok(None),
        Err(e) => return Err(e.context(format!("reading {}", path.display()))),
    };
    let read = (|| {
        let read: In = serde_json::from_slice(&text)?;
        anyhow::ensure!(read.version == VERSION, "unknown version {}", read.version);
        let cells = check(&read.patterns)?;
        Ok((read.patterns, cells))
    })();
    let (patterns, cells) = read.with_context(|| format!("reading {}", path.display()))?;
    Ok(Some(ProjectDay {
        patterns,
        cells,
        ..ProjectDay::default()
    }))
}

/// The bytes of the file at `path`, none when it is gone. Only a regular
/// file is read: a symlink is not followed, so a file cannot make Davy
/// read another, and a FIFO does not hold up the reader.
fn read_capped(path: &Path) -> Result<Option<Vec<u8>>> {
    let gone = |e: &io::Error| e.kind() == io::ErrorKind::NotFound;
    match std::fs::symlink_metadata(path) {
        Ok(meta) => anyhow::ensure!(meta.is_file(), "not a regular file"),
        Err(e) if gone(&e) => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let mut options = File::options();
    options.read(true);
    // Whatever replaced the file since it was checked is not followed or
    // waited for either.
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(
        &mut options,
        nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK,
    );
    let file = match options.open(path) {
        Ok(file) => file,
        Err(e) if gone(&e) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let meta = file.metadata()?;
    anyhow::ensure!(meta.is_file(), "not a regular file");
    let too_big = || anyhow::anyhow!("over {MAX_FILE_BYTES} bytes");
    if meta.len() > MAX_FILE_BYTES {
        return Err(too_big());
    }
    let mut text = Vec::new();
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut text)?;
    if u64::try_from(text.len()).unwrap_or(u64::MAX) > MAX_FILE_BYTES {
        return Err(too_big());
    }
    Ok(Some(text))
}

/// Checks `patterns` against the bounds and returns its pairs of pattern
/// and service. A pattern can add [`super::OTHER_SERVICES`] past
/// [`MAX_CELLS`], so the pairs can exceed it by one per pattern.
fn check(patterns: &HashMap<u64, Entry>) -> Result<usize> {
    anyhow::ensure!(
        patterns.len() <= MAX_PATTERNS + 1,
        "more than {} patterns",
        MAX_PATTERNS + 1
    );
    let mut cells = 0;
    for entry in patterns.values() {
        anyhow::ensure!(
            entry.template.len() <= TEMPLATE_BYTES,
            "a template of more than {TEMPLATE_BYTES} bytes"
        );
        anyhow::ensure!(
            entry.example.len() <= EXAMPLE_BYTES,
            "an example of more than {EXAMPLE_BYTES} bytes"
        );
        anyhow::ensure!(
            entry.services.keys().all(|s| s.len() <= SERVICE_BYTES),
            "a service name of more than {SERVICE_BYTES} bytes"
        );
        cells += entry.services.len();
    }
    let most = MAX_CELLS + MAX_PATTERNS + 1;
    anyhow::ensure!(
        cells <= most,
        "more than {most} pairs of pattern and service"
    );
    Ok(cells)
}

/// Renames the file of `project` in `dir`, which could not be read, to a
/// name no reader takes for counters, so the writer neither reads it again
/// nor writes over it. A retention cut of the project removes it with the
/// rest of the project's files.
pub(super) fn set_aside(dir: &Path, project: u64) -> Result<PathBuf> {
    let from = dir.join(name(project));
    let to = dir.join(format!("{}{ASIDE}{}", name(project), now_ns()));
    std::fs::rename(&from, &to).with_context(|| format!("renaming {}", from.display()))?;
    Ok(to)
}

/// The counters of `day` as the bytes of its file.
pub(super) fn encode(day: &ProjectDay) -> Result<Vec<u8>> {
    let out = Out {
        version: VERSION,
        patterns: &day.patterns,
    };
    Ok(serde_json::to_vec(&out)?)
}

/// Replaces the counters of `project` in `dir` with `bytes`.
pub(super) fn save(dir: &Path, project: u64, bytes: &[u8]) -> Result<()> {
    write_durably(dir, &name(project), bytes)
}

/// Removes the files of the projects `cut` deletes the logs of, the
/// temporary copies a crash left among them. A file already gone is fine,
/// so the cut can run again until it holds. A file that cannot be removed
/// does not keep the others, and its error is returned.
pub(super) fn remove(dir: &Path, cut: &Cut) -> Result<()> {
    let mut first = None;
    for file in names(dir)? {
        let Some((project, _)) = project_of(&file) else {
            continue;
        };
        let gone = match cut {
            Cut::Only(gone) => gone.contains(&project),
            Cut::AllBut(kept) => !kept.contains(&project),
        };
        let path = dir.join(&file);
        if gone
            && let Err(e) = std::fs::remove_file(&path)
            && e.kind() != io::ErrorKind::NotFound
        {
            first.get_or_insert(
                anyhow::Error::from(e).context(format!("removing {}", path.display())),
            );
        }
    }
    first.map_or(Ok(()), Err)
}

/// Replaces `name` in `dir` with `bytes` so that a crash or a power loss
/// leaves the old file or the new one, never a part. The temporary file
/// has a name of its own, so a file left at that name by a crash or put
/// there by anyone is neither followed nor in the way.
fn write_durably(dir: &Path, name: &str, bytes: &[u8]) -> Result<()> {
    let tmp = dir.join(format!("{name}.{}{TEMPORARY}", uuid::Uuid::new_v4()));
    let written = (|| -> io::Result<()> {
        let mut file = File::options().write(true).create_new(true).open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&tmp, dir.join(name))
    })();
    if let Err(e) = written {
        let e = anyhow::Error::from(e).context(format!("writing {}", dir.join(name).display()));
        return Err(match std::fs::remove_file(&tmp) {
            Err(left) if left.kind() != io::ErrorKind::NotFound => {
                e.context(format!("{} is left: {left}", tmp.display()))
            }
            _ => e,
        });
    }
    // The rename is durable only once the directory that holds it is.
    File::open(dir)?.sync_all()?;
    Ok(())
}
