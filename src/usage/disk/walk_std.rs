//! The walk of `walk.rs` for systems without `openat`: by path, so a
//! directory swapped for a link between the check and the read is
//! followed. It still reads at most [`MAX_ENTRIES`] entries.

use anyhow::{Context as _, Result, bail};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

pub const MAX_ENTRIES: u64 = 1_000_000;
const MAX_DEPTH: u32 = 32;

pub struct Opened(PathBuf);

pub struct Walk {
    limit: u64,
    left: u64,
}

impl Walk {
    pub fn new(data: &Path, limit: u64) -> Result<(Self, Opened)> {
        Ok((Self { limit, left: limit }, Opened(data.to_owned())))
    }

    pub fn names(&mut self, dir: &Opened) -> Result<Vec<String>> {
        let read = match fs::read_dir(&dir.0) {
            Ok(read) => read,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e).with_context(|| format!("reading {}", dir.0.display())),
        };
        let mut names = Vec::new();
        for entry in read {
            if self.left == 0 {
                bail!("the data directory has more than {} entries", self.limit);
            }
            self.left -= 1;
            names.push(entry?.file_name().to_string_lossy().into_owned());
        }
        Ok(names)
    }

    pub fn open(&self, dir: &Opened, name: &str) -> Result<Option<Opened>> {
        let path = dir.0.join(name);
        Ok(fs::symlink_metadata(&path)
            .is_ok_and(|m| m.is_dir())
            .then_some(Opened(path)))
    }

    pub fn size(&mut self, dir: &Opened, name: &str) -> Result<u64> {
        self.size_at(dir, name, 0)
    }

    fn size_at(&mut self, dir: &Opened, name: &str, depth: u32) -> Result<u64> {
        let path = dir.0.join(name);
        let meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e).with_context(|| format!("measuring {}", path.display())),
        };
        let mut total = meta.len();
        if meta.is_dir() {
            if depth >= MAX_DEPTH {
                bail!("the data directory is more than {MAX_DEPTH} directories deep");
            }
            let sub = Opened(path);
            for child in self.names(&sub)? {
                total = total.saturating_add(self.size_at(&sub, &child, depth + 1)?);
            }
        }
        Ok(total)
    }
}
