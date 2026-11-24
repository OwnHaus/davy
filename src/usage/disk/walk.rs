//! A walk over the data directory that never follows a symbolic link and
//! stays on the filesystem of the data directory. Each directory is opened
//! relative to its parent with `O_NOFOLLOW`, so a directory swapped for a
//! link during the walk is not entered. What it may not read or may not
//! reach in its limits it leaves out and names in [`Gaps`], so one
//! directory such as `lost+found` cannot fail the measurement.

use super::{Gaps, Limits};
use anyhow::{Context as _, Result};
use nix::{
    dir::Dir,
    errno::Errno,
    fcntl::{AtFlags, OFlag, openat},
    sys::stat::{FileStat, Mode, SFlag, fstat, fstatat},
};
use std::{
    ffi::OsStr,
    os::{
        fd::{AsFd, OwnedFd},
        unix::ffi::OsStrExt as _,
    },
    path::{Path, PathBuf},
    time::Instant,
};

/// Directories below the data directory a walk enters at most.
pub const MAX_DEPTH: u32 = 32;
/// Entries between two reads of the clock.
const CLOCK_EVERY: u64 = 256;

/// An open directory and its path under the data directory.
pub struct Opened {
    fd: OwnedFd,
    path: PathBuf,
}

pub struct Walk {
    /// The filesystem of the data directory.
    dev: nix::libc::dev_t,
    limits: Limits,
    start: Instant,
    read: u64,
    pub gaps: Gaps,
}

impl Walk {
    /// Opens the data directory `data`, which may be a link, and the walk
    /// under it.
    pub fn new(data: &Path, limits: Limits) -> Result<(Self, Opened)> {
        let flags = OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC;
        let fd = nix::fcntl::open(data, flags, Mode::empty())
            .with_context(|| format!("opening {}", data.display()))?;
        let walk = Self {
            dev: fstat(&fd)?.st_dev,
            limits,
            start: Instant::now(),
            read: 0,
            gaps: Gaps::default(),
        };
        let root = Opened {
            fd,
            path: PathBuf::new(),
        };
        Ok((walk, root))
    }

    /// Calls `f` with each name in `dir`, until the walk reaches a limit.
    pub fn each(
        &mut self,
        dir: &Opened,
        mut f: impl FnMut(&mut Self, &OsStr) -> Result<()>,
    ) -> Result<()> {
        for entry in Dir::from_fd(dir.fd.try_clone()?)?.iter() {
            let entry = entry.with_context(|| format!("reading {}", shown(&dir.path)))?;
            let name = OsStr::from_bytes(entry.file_name().to_bytes());
            if name == "." || name == ".." {
                continue;
            }
            if !self.another() {
                break;
            }
            f(self, name)?;
        }
        Ok(())
    }

    /// Whether the walk may read one more entry. Past a limit it says why
    /// in `gaps.stopped`.
    fn another(&mut self) -> bool {
        if self.gaps.stopped.is_some() {
            return false;
        }
        if self.read >= self.limits.entries {
            self.gaps.stopped = Some(format!("it read {} entries, its limit", self.read));
            return false;
        }
        if self.read.is_multiple_of(CLOCK_EVERY) && self.start.elapsed() > self.limits.time {
            self.gaps.stopped = Some(format!(
                "it took more than {} s, its limit",
                self.limits.time.as_secs()
            ));
            return false;
        }
        self.read += 1;
        true
    }

    /// The directory `name` in `dir`, `None` when it is not a directory, is
    /// gone, or is left out: one Davy may not read or one on another
    /// filesystem.
    pub fn open(&mut self, dir: &Opened, name: &OsStr) -> Result<Option<Opened>> {
        let flags = OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
        let path = dir.path.join(name);
        let fd = match openat(dir.fd.as_fd(), name, flags, Mode::empty()) {
            Ok(fd) => fd,
            // FreeBSD answers a link under `O_NOFOLLOW` with `EMLINK`.
            Err(Errno::ENOENT | Errno::ENOTDIR | Errno::ELOOP | Errno::EMLINK) => return Ok(None),
            Err(Errno::EACCES | Errno::EPERM) => {
                self.gaps.skip(&path, "permission denied");
                return Ok(None);
            }
            Err(e) => return Err(e).with_context(|| format!("opening {}", shown(&path))),
        };
        if fstat(&fd)?.st_dev != self.dev {
            self.gaps.skip(&path, "on another filesystem");
            return Ok(None);
        }
        Ok(Some(Opened { fd, path }))
    }

    /// Whether `name` in `dir` is a directory, not a link to one.
    pub fn is_dir(dir: &Opened, name: &OsStr) -> bool {
        fstatat(dir.fd.as_fd(), name, AtFlags::AT_SYMLINK_NOFOLLOW).is_ok_and(|s| is_dir(&s))
    }

    /// What `name` in `dir` and everything under it takes on disk, 0 when
    /// it is gone.
    pub fn size(&mut self, dir: &Opened, name: &OsStr) -> Result<u64> {
        self.size_at(dir, name, 0)
    }

    fn size_at(&mut self, dir: &Opened, name: &OsStr, depth: u32) -> Result<u64> {
        let stat = match fstatat(dir.fd.as_fd(), name, AtFlags::AT_SYMLINK_NOFOLLOW) {
            Ok(stat) => stat,
            Err(Errno::ENOENT) => return Ok(0),
            Err(Errno::EACCES | Errno::EPERM) => {
                self.gaps.skip(&dir.path.join(name), "permission denied");
                return Ok(0);
            }
            Err(e) => {
                return Err(e)
                    .with_context(|| format!("measuring {}", shown(&dir.path.join(name))));
            }
        };
        let mut total = u64::try_from(stat.st_blocks)
            .unwrap_or(0)
            .saturating_mul(512);
        if !is_dir(&stat) {
            return Ok(total);
        }
        if depth >= MAX_DEPTH {
            let why = format!("more than {MAX_DEPTH} directories deep");
            self.gaps.skip(&dir.path.join(name), &why);
            return Ok(total);
        }
        if let Some(sub) = self.open(dir, name)? {
            self.each(&sub, |walk, child| {
                total = total.saturating_add(walk.size_at(&sub, child, depth + 1)?);
                Ok(())
            })?;
        }
        Ok(total)
    }
}

fn is_dir(stat: &FileStat) -> bool {
    SFlag::from_bits_truncate(stat.st_mode) & SFlag::S_IFMT == SFlag::S_IFDIR
}

fn shown(path: &Path) -> String {
    if path.as_os_str().is_empty() {
        "the data directory".to_owned()
    } else {
        path.display().to_string()
    }
}
