//! `davy backup`. A backup holds
//! every secret of the server, so only whoever runs commands on the host
//! takes one: no MCP tool or HTTP route does. See docs/design.md.

use super::{
    Record, archive,
    take::{self, Taken},
};
use crate::text::size;
use anyhow::{Context as _, Result, ensure};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufWriter, Write},
    path::{Path, PathBuf},
    time::Instant,
};

#[derive(clap::Args)]
pub struct Backup {
    /// The data directory of the server, running or not.
    #[arg(long)]
    data: PathBuf,
    /// The file to write, outside the data directory, or - for stdout.
    #[arg(value_name = "OUT")]
    out: PathBuf,
}

/// What a backup says about itself when it is done.
pub const SECRETS: &str = "It holds the secrets of this server, such as its admin token, the SMTP URL, alert channel URLs, password hashes and sign-in sessions: keep it where only you can read it.";

pub fn backup(out: &mut impl Write, args: Backup) -> Result<()> {
    let (data, to) = (args.data, args.out);
    let start = Instant::now();
    let stdout = to == Path::new("-");
    let shown = if stdout {
        "stdout".to_owned()
    } else {
        to.display().to_string()
    };
    let (taken, written) = if stdout {
        let (w, taken) = take::take(&data, Counted::new(BufWriter::new(io::stdout().lock())))?;
        let written = w.n;
        w.inner
            .into_inner()
            .map_err(io::IntoInnerError::into_error)?
            .flush()?;
        (taken, written)
    } else {
        to_file(&data, &to)?
    };
    let record = Record {
        at_ms: taken.at_ms,
        davy_version: taken.header.davy_version.clone(),
        schema: taken.header.schema,
        files: taken.files,
        bytes: taken.bytes,
        written,
        to: shown.clone(),
    };
    // The backup is whole: a record that cannot be saved does not undo it.
    if let Err(e) = record.save(&data) {
        writeln!(
            io::stderr(),
            "warning: the backup is whole, but its record for davy doctor was not saved: {e:#}"
        )?;
    }
    let summary = summary(&data, &shown, &taken, written, start);
    if stdout {
        writeln!(io::stderr(), "{summary}")?;
    } else {
        writeln!(out, "{summary}")?;
    }
    Ok(())
}

/// Writes the backup to a file beside `to` and renames it into place once
/// it is whole and on disk, so `to` is never half a backup.
fn to_file(data: &Path, to: &Path) -> Result<(Taken, u64)> {
    let name = to
        .file_name()
        .context("OUT names no file")?
        .to_string_lossy();
    let dir = match to.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let real = dir
        .canonicalize()
        .with_context(|| format!("reading {}", dir.display()))?;
    let data_real = data
        .canonicalize()
        .with_context(|| format!("reading {}", data.display()))?;
    ensure!(
        !real.starts_with(&data_real),
        "{} is inside the data directory: write the backup somewhere else",
        to.display()
    );
    let tmp = dir.join(format!(".{name}.tmp-{}", std::process::id()));
    leftovers(dir, &name);
    let mut o = OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, archive::FILE_MODE);
    let file = o
        .open(&tmp)
        .with_context(|| format!("creating {}", tmp.display()))?;
    // Held until the rename, so the next backup to the same file knows
    // this one is not a leftover.
    file.lock()
        .with_context(|| format!("locking {}", tmp.display()))?;
    let written = (|| {
        let (w, taken) = take::take(data, Counted::new(BufWriter::new(file)))?;
        let written = w.n;
        let file = w
            .inner
            .into_inner()
            .map_err(io::IntoInnerError::into_error)?;
        file.sync_all()?;
        fs::rename(&tmp, to).with_context(|| format!("renaming the backup to {}", to.display()))?;
        File::open(dir).and_then(|d| d.sync_all())?;
        Ok((taken, written))
    })();
    if written.is_err() {
        drop(fs::remove_file(&tmp));
    }
    written
}

/// Removes the files `.NAME.tmp-PID` in `dir` that killed backups to the
/// same `NAME` left: those whose lock no process holds. Errors are
/// ignored: the next backup tries again.
fn leftovers(dir: &Path, name: &str) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let prefix = format!(".{name}.tmp-");
    for entry in entries.flatten() {
        let found = entry.file_name();
        let Some(pid) = found.to_str().and_then(|f| f.strip_prefix(&prefix)) else {
            continue;
        };
        if !pid.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        if let Ok(file) = File::open(entry.path())
            && file.try_lock().is_ok()
        {
            drop(fs::remove_file(entry.path()));
        }
    }
}

fn summary(data: &Path, to: &str, taken: &Taken, written: u64, start: Instant) -> String {
    let mut s = format!(
        "backed up {} files, {}, of {} to {} as {} in {:.1} s; the log writer paused for {} ms.\n{SECRETS}",
        taken.files,
        size(taken.bytes),
        data.display(),
        to,
        size(written),
        start.elapsed().as_secs_f64(),
        taken.pause.as_millis()
    );
    if !taken.skipped.is_empty() {
        s.push_str("\nleft out, as neither files nor directories: ");
        s.push_str(&taken.skipped.join(", "));
    }
    if !taken.long.is_empty() {
        s.push_str("\nleft out, as names longer than the 100 bytes a backup holds: ");
        s.push_str(&taken.long.join(", "));
    }
    s
}

/// A writer that counts what it writes.
struct Counted<W> {
    inner: W,
    n: u64,
}

impl<W> Counted<W> {
    const fn new(inner: W) -> Self {
        Self { inner, n: 0 }
    }
}

impl<W: Write> Write for Counted<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.n += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
#[path = "cli_tests.rs"]
mod tests;
