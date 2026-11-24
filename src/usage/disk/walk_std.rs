//! The walk of `walk.rs` for systems without `openat`: by path, so a
//! directory swapped for a link between the check and the read is
//! followed. It sizes files by their length, not their allocated blocks,
//! and cannot tell another filesystem. It keeps the limits and the gaps of
//! `walk.rs`.

use super::{Gaps, Limits};
use anyhow::{Context as _, Result};
use std::{
    ffi::OsStr,
    fs, io,
    path::{Path, PathBuf},
    time::Instant,
};

pub const MAX_DEPTH: u32 = 32;

/// A directory: its full path and its path under the data directory.
pub struct Opened {
    full: PathBuf,
    path: PathBuf,
}

pub struct Walk {
    limits: Limits,
    start: Instant,
    read: u64,
    pub gaps: Gaps,
}

fn denied(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::PermissionDenied
}

#[expect(
    clippy::unnecessary_wraps,
    clippy::unused_self,
    reason = "the signatures of walk.rs, whose walk can fail where this one cannot and reads its own state"
)]
impl Walk {
    pub fn new(data: &Path, limits: Limits) -> Result<(Self, Opened)> {
        let walk = Self {
            limits,
            start: Instant::now(),
            read: 0,
            gaps: Gaps::default(),
        };
        let root = Opened {
            full: data.to_owned(),
            path: PathBuf::new(),
        };
        Ok((walk, root))
    }

    pub fn each(
        &mut self,
        dir: &Opened,
        mut f: impl FnMut(&mut Self, &OsStr) -> Result<()>,
    ) -> Result<()> {
        let read = match fs::read_dir(&dir.full) {
            Ok(read) => read,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) if denied(&e) => {
                self.gaps.skip(&dir.path, "permission denied");
                return Ok(());
            }
            Err(e) => return Err(e).with_context(|| format!("reading {}", dir.full.display())),
        };
        for entry in read {
            let entry = entry?;
            if !self.another() {
                break;
            }
            f(self, &entry.file_name())?;
        }
        Ok(())
    }

    fn another(&mut self) -> bool {
        if self.gaps.stopped.is_some() {
            return false;
        }
        if self.read >= self.limits.entries {
            self.gaps.stopped = Some(format!("it read {} entries, its limit", self.read));
            return false;
        }
        if self.start.elapsed() > self.limits.time {
            self.gaps.stopped = Some(format!(
                "it took more than {} s, its limit",
                self.limits.time.as_secs()
            ));
            return false;
        }
        self.read += 1;
        true
    }

    pub fn open(&mut self, dir: &Opened, name: &OsStr) -> Result<Option<Opened>> {
        let full = dir.full.join(name);
        Ok(fs::symlink_metadata(&full)
            .is_ok_and(|m| m.is_dir())
            .then(|| Opened {
                full,
                path: dir.path.join(name),
            }))
    }

    pub fn is_dir(dir: &Opened, name: &OsStr) -> bool {
        fs::symlink_metadata(dir.full.join(name)).is_ok_and(|m| m.is_dir())
    }

    pub fn size(&mut self, dir: &Opened, name: &OsStr) -> Result<u64> {
        self.size_at(dir, name, 0)
    }

    fn size_at(&mut self, dir: &Opened, name: &OsStr, depth: u32) -> Result<u64> {
        let full = dir.full.join(name);
        let meta = match fs::symlink_metadata(&full) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(e) if denied(&e) => {
                self.gaps.skip(&dir.path.join(name), "permission denied");
                return Ok(0);
            }
            Err(e) => return Err(e).with_context(|| format!("measuring {}", full.display())),
        };
        let mut total = meta.len();
        if !meta.is_dir() {
            return Ok(total);
        }
        if depth >= MAX_DEPTH {
            let why = format!("more than {MAX_DEPTH} directories deep");
            self.gaps.skip(&dir.path.join(name), &why);
            return Ok(total);
        }
        let sub = Opened {
            full,
            path: dir.path.join(name),
        };
        self.each(&sub, |walk, child| {
            total = total.saturating_add(walk.size_at(&sub, child, depth + 1)?);
            Ok(())
        })?;
        Ok(total)
    }
}
