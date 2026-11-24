//! The files of the pattern counters: one per project and day in the day's
//! directory, so a flush rewrites only the projects that changed, and a
//! cut of projects removes their files without reading any.

use super::{Cut, Entry, ProjectDay};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs::File,
    io::{self, Write},
    path::Path,
};

const PREFIX: &str = "patterns-";
const SUFFIX: &str = ".json";
const VERSION: u32 = 1;

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

/// The projects with counters in `dir`.
pub(super) fn projects(dir: &Path) -> Result<Vec<u64>> {
    Ok(names(dir)?
        .iter()
        .filter_map(|n| project_of(n))
        .filter_map(|(project, file)| file.then_some(project))
        .collect())
}

/// The counters of `project` in `dir`, if it has any.
pub(super) fn load(dir: &Path, project: u64) -> Result<Option<ProjectDay>> {
    let path = dir.join(name(project));
    let text = match std::fs::read(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(anyhow::Error::from(e).context(format!("reading {}", path.display()))),
    };
    let read: In =
        serde_json::from_slice(&text).with_context(|| format!("reading {}", path.display()))?;
    anyhow::ensure!(
        read.version == VERSION,
        "{}: unknown version {}",
        path.display(),
        read.version
    );
    let cells = read.patterns.values().map(|e| e.services.len()).sum();
    Ok(Some(ProjectDay {
        patterns: read.patterns,
        cells,
        ..ProjectDay::default()
    }))
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
    let tmp = dir.join(format!("{name}.{}.tmp", uuid::Uuid::new_v4()));
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
