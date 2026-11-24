//! A walk over the data directory that never follows a symbolic link, stays
//! on the filesystem of the data directory and stops after
//! [`MAX_ENTRIES`] entries. Each directory is opened relative to its parent
//! with `O_NOFOLLOW`, so a directory swapped for a link during the walk is
//! not entered.

use anyhow::{Context as _, Result, bail};
use nix::{
    dir::Dir,
    errno::Errno,
    fcntl::{AtFlags, OFlag, openat},
    sys::stat::{FileStat, Mode, SFlag, fstat, fstatat},
};
use std::{
    os::fd::{AsFd, OwnedFd},
    path::Path,
};

/// Entries one measurement reads at most, so a directory tree someone
/// filled with files cannot keep a request busy for long.
pub const MAX_ENTRIES: u64 = 1_000_000;
/// Directories below the data directory a walk enters at most.
const MAX_DEPTH: u32 = 32;

/// An open directory.
pub struct Opened(OwnedFd);

pub struct Walk {
    /// The filesystem of the data directory.
    dev: FileStat,
    limit: u64,
    left: u64,
}

impl Walk {
    /// Opens the data directory `data`, which may be a link, and the walk
    /// that reads at most `limit` entries under it.
    pub fn new(data: &Path, limit: u64) -> Result<(Self, Opened)> {
        let flags = OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC;
        let fd = nix::fcntl::open(data, flags, Mode::empty())
            .with_context(|| format!("opening {}", data.display()))?;
        let dev = fstat(&fd)?;
        Ok((
            Self {
                dev,
                limit,
                left: limit,
            },
            Opened(fd),
        ))
    }

    /// The names in `dir`.
    pub fn names(&mut self, dir: &Opened) -> Result<Vec<String>> {
        let mut names = Vec::new();
        for entry in Dir::from_fd(dir.0.try_clone()?)? {
            let name = entry?.file_name().to_string_lossy().into_owned();
            if name == "." || name == ".." {
                continue;
            }
            if self.left == 0 {
                bail!("the data directory has more than {} entries", self.limit);
            }
            self.left -= 1;
            names.push(name);
        }
        Ok(names)
    }

    /// The directory `name` in `dir`, `None` when it is not a directory of
    /// the same filesystem, or is gone.
    pub fn open(&self, dir: &Opened, name: &str) -> Result<Option<Opened>> {
        let flags = OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
        let fd = match openat(dir.0.as_fd(), name, flags, Mode::empty()) {
            Ok(fd) => fd,
            Err(Errno::ENOENT | Errno::ENOTDIR | Errno::ELOOP) => return Ok(None),
            Err(e) => return Err(e).with_context(|| format!("opening {name}")),
        };
        Ok((fstat(&fd)?.st_dev == self.dev.st_dev).then_some(Opened(fd)))
    }

    /// What `name` in `dir` and everything under it takes on disk, 0 when
    /// it is gone.
    pub fn size(&mut self, dir: &Opened, name: &str) -> Result<u64> {
        self.size_at(dir, name, 0)
    }

    fn size_at(&mut self, dir: &Opened, name: &str, depth: u32) -> Result<u64> {
        let stat = match fstatat(dir.0.as_fd(), name, AtFlags::AT_SYMLINK_NOFOLLOW) {
            Ok(stat) => stat,
            Err(Errno::ENOENT) => return Ok(0),
            Err(e) => return Err(e).with_context(|| format!("measuring {name}")),
        };
        let mut total = u64::try_from(stat.st_blocks)
            .unwrap_or(0)
            .saturating_mul(512);
        if !is_dir(&stat) {
            return Ok(total);
        }
        if depth >= MAX_DEPTH {
            bail!("the data directory is more than {MAX_DEPTH} directories deep");
        }
        if let Some(sub) = self.open(dir, name)? {
            for child in self.names(&sub)? {
                total = total.saturating_add(self.size_at(&sub, &child, depth + 1)?);
            }
        }
        Ok(total)
    }
}

fn is_dir(stat: &FileStat) -> bool {
    SFlag::from_bits_truncate(stat.st_mode) & SFlag::S_IFMT == SFlag::S_IFDIR
}
