//! `davy backup`, `davy backup verify` and `davy restore`. A backup holds
//! every secret of the server, so only whoever runs commands on the host
//! takes one: no MCP tool or HTTP route does. See docs/design.md.

use super::{
    Record,
    archive::{self, Check, Header},
    restore::{self as put, Restored},
    take::{self, Taken},
};
use crate::{store::SCHEMA_VERSION, text::size};
use anyhow::{Context as _, Result, ensure};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
    time::Instant,
};

#[derive(clap::Args)]
#[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
pub struct Backup {
    #[command(subcommand)]
    command: Option<Verb>,
    /// The data directory of the server, running or not.
    #[arg(long, required = true)]
    data: Option<PathBuf>,
    /// The file to write, outside the data directory, or - for stdout.
    #[arg(required = true, value_name = "OUT")]
    out: Option<PathBuf>,
}

#[derive(clap::Subcommand)]
enum Verb {
    /// Reads a backup through and checks every file against its manifest,
    /// without restoring it.
    Verify {
        /// The backup, or - for stdin.
        file: PathBuf,
    },
}

#[derive(clap::Args)]
pub struct Restore {
    /// The data directory to restore into: missing or empty, and no server
    /// on it.
    #[arg(long)]
    data: PathBuf,
    /// The backup, or - for stdin.
    #[arg(value_name = "IN")]
    input: PathBuf,
    /// Restore into a directory that holds something, after moving what it
    /// holds into DATA/.replaced-TIME-PID.
    #[arg(long)]
    replace: bool,
}

/// What a backup says about itself when it is done.
pub const SECRETS: &str = "It holds the secrets of this server, such as its admin token, the SMTP URL, alert channel URLs, password hashes and sign-in sessions: keep it where only you can read it.";

pub fn backup(out: &mut impl Write, args: Backup) -> Result<()> {
    let (Some(data), Some(to)) = (args.data, args.out) else {
        let Some(Verb::Verify { file }) = args.command else {
            unreachable!("clap requires --data and OUT without a subcommand")
        };
        return verify(out, &file);
    };
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

fn verify(out: &mut impl Write, file: &Path) -> Result<()> {
    let (header, manifest) = archive::read(input(file)?, &mut Check)?;
    writeln!(
        out,
        "the backup is whole: {} files, {}, {}",
        manifest.files.len(),
        size(manifest.bytes),
        about(&header)
    )?;
    if header.schema > SCHEMA_VERSION {
        writeln!(
            out,
            "this Davy {} knows schema versions up to {SCHEMA_VERSION}: restore it with Davy {} or newer",
            env!("CARGO_PKG_VERSION"),
            header.davy_version
        )?;
    }
    Ok(())
}

pub fn restore(out: &mut impl Write, args: &Restore) -> Result<()> {
    if let Some(aside) = put::finish(&args.data)? {
        writeln!(
            out,
            "finished the restore into {} that stopped halfway; {}. Nothing was read from {}: to restore it over the one just finished, run davy restore again with --replace",
            args.data.display(),
            super::swap::replaced(&aside),
            args.input.display()
        )?;
        return Ok(());
    }
    let r: Restored = put::restore(input(&args.input)?, &args.data, args.replace)?;
    writeln!(
        out,
        "restored {} files, {}, into {}: {}",
        r.files,
        size(r.bytes),
        args.data.display(),
        about(&r.header)
    )?;
    if let Some(aside) = &r.replaced {
        writeln!(
            out,
            "what {} held is in {}",
            args.data.display(),
            aside.display()
        )?;
    }
    if r.header.schema < SCHEMA_VERSION {
        writeln!(
            out,
            "davy serve upgrades its database from schema version {} to {SCHEMA_VERSION} when it starts",
            r.header.schema
        )?;
    }
    Ok(())
}

fn about(h: &Header) -> String {
    format!(
        "taken {} by Davy {} at schema version {}",
        h.created_at, h.davy_version, h.schema
    )
}

fn input(path: &Path) -> Result<Box<dyn Read>> {
    if path == Path::new("-") {
        return Ok(Box::new(BufReader::new(io::stdin().lock())));
    }
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    Ok(Box::new(BufReader::with_capacity(1 << 16, file)))
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
