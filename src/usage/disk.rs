//! What Davy's files take on disk, measured when asked by walking the data
//! directory, and the filesystem's free and total space.

use crate::{logs, store};
use anyhow::{Context as _, Result};
use jiff::civil::Date;
use std::{fs, io, path::Path};

/// The directories `sourcemaps::Store` keeps uploads in.
const UPLOADS: [&str; 2] = ["sourcemaps", "chunks"];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Disk {
    /// Each closed log day, oldest first.
    pub closed: Vec<(Date, u64)>,
    /// Today's log day, and any after it.
    pub today: u64,
    /// The database with its WAL and shared memory files.
    pub sqlite: u64,
    /// Source maps and upload chunks.
    pub uploads: u64,
    /// Everything else under the data directory.
    pub other: u64,
    pub free: u64,
    pub total: u64,
}

impl Disk {
    pub fn logs(&self) -> u64 {
        self.closed.iter().map(|(_, b)| b).sum::<u64>() + self.today
    }
    /// What Davy's files take.
    pub fn davy(&self) -> u64 {
        self.logs() + self.fixed()
    }
    /// What is not logs.
    pub fn fixed(&self) -> u64 {
        self.sqlite + self.uploads + self.other
    }
    /// What the filesystem uses, Davy's files and any others.
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.free)
    }
}

/// Measures `data`, with `today` the UTC day that is still open.
pub fn measure(data: &Path, today: Date) -> Result<Disk> {
    let mut disk = Disk {
        free: fs4::available_space(data)
            .with_context(|| format!("free space of {}", data.display()))?,
        total: fs4::total_space(data).with_context(|| format!("size of {}", data.display()))?,
        ..Disk::default()
    };
    let db = store::DB_FILE;
    let sqlite = [db.to_owned(), format!("{db}-wal"), format!("{db}-shm")];
    let logs = logs::dir(data);
    for entry in entries(data)? {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path == logs {
            measure_logs(&path, today, &mut disk)?;
        } else if sqlite.iter().any(|s| *s == name) {
            disk.sqlite += size(&path)?;
        } else if UPLOADS.contains(&&*name) {
            disk.uploads += size(&path)?;
        } else {
            disk.other += size(&path)?;
        }
    }
    disk.closed.sort_unstable();
    Ok(disk)
}

fn measure_logs(dir: &Path, today: Date, disk: &mut Disk) -> Result<()> {
    for entry in entries(dir)? {
        let path = entry.path();
        let bytes = size(&path)?;
        match logs::store::parse_day(&entry.file_name().to_string_lossy()) {
            Some(day) if day < today => disk.closed.push((day, bytes)),
            Some(_) => disk.today += bytes,
            None => disk.other += bytes,
        }
    }
    Ok(())
}

/// The entries of `dir`, none when it does not exist.
fn entries(dir: &Path) -> Result<Vec<fs::DirEntry>> {
    match fs::read_dir(dir) {
        Ok(read) => Ok(read.collect::<io::Result<_>>()?),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e).with_context(|| format!("reading {}", dir.display())),
    }
}

/// What `path` and everything under it takes on disk. Symbolic links are
/// not followed, and a file retention deletes during the walk counts 0.
fn size(path: &Path) -> Result<u64> {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e).with_context(|| format!("measuring {}", path.display())),
    };
    let mut total = allocated(&meta);
    if meta.is_dir() {
        for entry in entries(path)? {
            total += size(&entry.path())?;
        }
    }
    Ok(total)
}

/// Bytes the file takes on disk, which for a sparse or compressed file is
/// less than its length.
#[cfg(unix)]
fn allocated(meta: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt as _;
    meta.blocks() * 512
}

#[cfg(not(unix))]
fn allocated(meta: &fs::Metadata) -> u64 {
    meta.len()
}
