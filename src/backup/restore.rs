//! `davy restore`: a backup checked whole in a staging directory inside the
//! data directory, then put in place by [`swap`], which a crash cannot
//! leave halfway. Nothing is in place before every file matched the
//! manifest.

use super::{
    archive::{self, FILE_MODE, Header, Sink},
    link, pause,
    swap::{self, Claim, REPLACED, STAGING},
};
use crate::store::{self, DB_FILE, SCHEMA_VERSION};
use anyhow::{Context as _, Result, bail, ensure};
use std::{
    fs::{self, OpenOptions},
    io::{self, Read},
    path::{Path, PathBuf},
};

/// What a restore put in place.
#[derive(Debug)]
pub struct Restored {
    pub header: Header,
    pub files: usize,
    pub bytes: u64,
    /// Where what the data directory held went, with `--replace`.
    pub replaced: Option<PathBuf>,
}

/// Restores the backup read from `input` into the data directory `data`,
/// which must be empty or missing unless `replace`, and in use by no
/// process. A restore into `data` that stopped halfway is finished first.
pub fn restore(input: impl Read, data: &Path, replace: bool) -> Result<Restored> {
    // Before the claim, which would leave a directory a server run as
    // another user cannot open.
    if data.exists() {
        super::owned(data, "restore")?;
    }
    let _claim = Claim::restore(data)?;
    swap::finish(data)?;
    prepare(data, replace)?;
    let staging = data.join(format!(
        "{STAGING}{}-{}",
        std::process::id(),
        crate::time::now_ns()
    ));
    link::mkdir(&staging)?;
    let checked = extract(input, data, &staging).and_then(|read| {
        swap::seal(data, &staging)?;
        // A server older than this Davy takes no claim: look again.
        in_use(data)?;
        Ok(read)
    });
    let (header, manifest) = match checked {
        Ok(read) => read,
        Err(e) => {
            drop(fs::remove_dir_all(&staging));
            return Err(e);
        }
    };
    let moved = swap::commit(data, &staging)?;
    Ok(Restored {
        header,
        files: manifest.files.len(),
        bytes: manifest.bytes,
        // Made only when something was moved into it.
        replaced: moved.exists().then_some(moved),
    })
}

/// Finishes a restore into `data` that stopped halfway, past its commit
/// point, if one did. Returns where what the data directory held went.
pub fn finish(data: &Path) -> Result<Option<PathBuf>> {
    if !data.exists() {
        return Ok(None);
    }
    super::owned(data, "restore")?;
    let _claim = Claim::restore(data)?;
    swap::finish(data)
}

/// Reads the backup into `staging` in `data` and checks it, the
/// database's schema against the header's too.
fn extract(input: impl Read, data: &Path, staging: &Path) -> Result<(Header, archive::Manifest)> {
    let mut sink = Extract { data, staging };
    let (header, manifest) = archive::read(input, &mut sink)?;
    let schema = user_version(&staging.join(DB_FILE))?;
    ensure!(
        schema == header.schema,
        "the backup's database is at schema version {schema}, its header says {}",
        header.schema
    );
    Ok((header, manifest))
}

/// The schema version of the database at `path`, read from its header:
/// opening it would leave SQLite's files beside it.
fn user_version(path: &Path) -> Result<u32> {
    let mut head = [0; 100];
    match fs::File::open(path) {
        Ok(mut f) => f
            .read_exact(&mut head)
            .with_context(|| format!("the backup's {DB_FILE} is not a database"))?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => bail!("the backup holds no {DB_FILE}"),
        Err(e) => return Err(e.into()),
    }
    ensure!(
        head.starts_with(b"SQLite format 3\0"),
        "the backup's {DB_FILE} is not a database"
    );
    Ok(u32::from_be_bytes([head[60], head[61], head[62], head[63]]))
}

/// Refuses a data directory another process uses, and one that holds
/// anything unless `replace`.
fn prepare(data: &Path, replace: bool) -> Result<()> {
    in_use(data)?;
    if !swap::held(data)?.is_empty() && !replace {
        let shown = data.display();
        bail!(
            "{shown} is not empty: restore into a new directory, or pass --replace to move what it holds into {shown}/{REPLACED}TIME-PID first"
        );
    }
    Ok(())
}

/// Refuses a data directory a log writer or another process uses, which a
/// server older than this Davy does without the claim.
fn in_use(data: &Path) -> Result<()> {
    let shown = data.display();
    ensure!(
        !pause::running(&crate::logs::dir(data))?,
        "a Davy server runs on {shown}: stop it before you restore into it"
    );
    ensure!(
        !data.join(DB_FILE).exists() || !store::in_use(data)?,
        "another process has {} open: stop it before you restore into {shown}",
        data.join(DB_FILE).display()
    );
    Ok(())
}

/// Writes each file of the backup under the staging directory, for the
/// owner only, and to disk before the files move into place.
struct Extract<'a> {
    data: &'a Path,
    staging: &'a Path,
}

impl Sink for Extract<'_> {
    fn header(&mut self, header: &Header) -> Result<()> {
        ensure!(
            header.schema <= SCHEMA_VERSION,
            "the backup is from Davy {}, at schema version {}, and this Davy {} knows versions up to {SCHEMA_VERSION}: restore it with Davy {} or newer",
            header.davy_version,
            header.schema,
            env!("CARGO_PKG_VERSION"),
            header.davy_version
        );
        let free = fs4::available_space(self.data)
            .with_context(|| format!("reading the free space of {}", self.data.display()))?;
        ensure!(
            free >= header.bytes,
            "the backup holds {} and {} has {} free: make room and restore again",
            crate::text::size(header.bytes),
            self.data.display(),
            crate::text::size(free)
        );
        Ok(())
    }

    fn file(&mut self, path: &Path, _: u64, data: &mut dyn Read) -> Result<()> {
        let to = self.staging.join(path);
        if let Some(dir) = to.parent() {
            link::mkdir_all(dir)?;
        }
        let mut o = OpenOptions::new();
        o.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut o, FILE_MODE);
        let mut file = o.open(&to)?;
        io::copy(data, &mut file)?;
        file.sync_all()?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "restore_tests.rs"]
mod tests;
