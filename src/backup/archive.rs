//! The backup file: a tar stream compressed with zstd. The first entry is
//! [`HEADER`], the files of the data directory follow under `data/`, and
//! [`MANIFEST`] ends it with each file's size and BLAKE3 hash. See "Backup
//! and restore" in docs/design.md.

use anyhow::{Context as _, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
};

/// The format this Davy writes and the only one it reads.
pub const FORMAT: u32 = 1;
pub const HEADER: &str = "davy-backup.json";
pub const MANIFEST: &str = "manifest.json";
/// The directory in the archive that holds the data directory's files.
pub const DATA: &str = "data";
/// zstd level 1: most bytes are segments and payloads zstd already
/// compressed, and level 3 took twice the time for the same size to two
/// digits, see the measurements in docs/design.md.
const LEVEL: i32 = 1;
const MAX_HEADER: u64 = 64 * 1024;
/// About 150 bytes per file, so over a million files. A manifest is held
/// to less: what the files before it can take, see [`manifest_cap`].
const MAX_MANIFEST: u64 = 256 * 1024 * 1024;
/// Names a backup never holds at the top of the data directory: SQLite's
/// files beside the database, which would pair with a database they do
/// not belong to, the record of the last backup, and [`LOST_FOUND`].
const NOT_AT_TOP: [&str; 6] = [
    "davy.sqlite-wal",
    "davy.sqlite-shm",
    "davy.sqlite-journal",
    "davy.sqlite.lock",
    super::RECORD,
    LOST_FOUND,
];
/// What `mkfs.ext4` makes at the top of a volume. A data directory that is
/// one has it, and a restore into one would find it in the way.
pub const LOST_FOUND: &str = "lost+found";
/// The longest name a tar header holds, `data/` included. A longer one
/// needs an extension entry, which the tar crate reads whole into memory
/// before any cap applies, so the writer writes none and the reader
/// refuses them. Davy's longest name is a source map's, under 70 bytes.
pub const MAX_NAME: usize = 100;
/// Files and directories are 0600 and 0700 in the data directory, which
/// `davy serve` makes with umask 077, and restore makes them so again.
pub const FILE_MODE: u32 = 0o600;

/// What [`HEADER`] holds: which Davy wrote the backup, which schema its
/// database has, and how many files and bytes follow, read before any file
/// so a restore can refuse early and hold the backup to them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Header {
    pub format: u32,
    pub davy_version: String,
    pub schema: u32,
    /// The moment the backup holds: when its snapshot of the database was
    /// opened.
    pub created_at: String,
    /// The most files that follow.
    pub files: u64,
    /// The most bytes the files hold together, before compression.
    pub bytes: u64,
}

impl Header {
    /// The header of a backup this Davy takes of the moment `at_ms`, of a
    /// database at `schema`, with at most `files` files of `bytes` bytes.
    pub fn now(schema: u32, at_ms: i64, files: u64, bytes: u64) -> Self {
        Self {
            format: FORMAT,
            davy_version: env!("CARGO_PKG_VERSION").to_owned(),
            schema,
            created_at: crate::time::rfc3339_ms(at_ms),
            files,
            bytes,
        }
    }
}

/// One file of [`MANIFEST`], its path relative to the data directory.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct File {
    pub path: String,
    pub size: u64,
    pub blake3: String,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub files: Vec<File>,
    /// The sum of the sizes, so a person reading the manifest sees it.
    pub bytes: u64,
}

/// Writes a backup to `W`: [`Writer::new`], then [`Writer::add`] for each
/// file, then [`Writer::finish`]. Dropped before `finish`, it leaves a
/// stream without a manifest, which [`read`] refuses.
pub struct Writer<W: Write> {
    tar: tar::Builder<zstd::stream::write::Encoder<'static, W>>,
    files: Vec<File>,
    mtime: u64,
    /// The header's files and bytes, which the files may not pass.
    most: (u64, u64),
    bytes: u64,
}

impl<W: Write> Writer<W> {
    pub fn new(out: W, header: &Header) -> Result<Self> {
        let mut zstd = zstd::stream::write::Encoder::new(out, LEVEL)?;
        zstd.include_checksum(true)?;
        let mtime = jiff::Timestamp::now().as_second().max(0).cast_unsigned();
        let mut w = Self {
            tar: tar::Builder::new(zstd),
            files: Vec::new(),
            mtime,
            most: (header.files, header.bytes),
            bytes: 0,
        };
        let json = serde_json::to_vec_pretty(header)?;
        w.entry(HEADER, json.len() as u64, &mut json.as_slice())?;
        Ok(w)
    }

    /// Adds the file at `path` in the data directory, `size` bytes read
    /// from `data`. Fails when `data` ends before `size`, which a file that
    /// shrank while it was read does, and reads no byte past it. Fails too
    /// past the files or bytes the header gives.
    pub fn add(&mut self, path: &Path, size: u64, data: impl Read) -> Result<()> {
        let name = relative(path)?;
        ensure!(
            (self.files.len() as u64) < self.most.0 && self.bytes + size <= self.most.1,
            "{name} makes the backup more than the {} files of {} bytes it listed: the data directory grew while it was written, take the backup again",
            self.most.0,
            self.most.1
        );
        self.bytes += size;
        let mut hashed = Hashed::new(data.take(size));
        self.entry(&format!("{DATA}/{name}"), size, &mut hashed)
            .with_context(|| format!("adding {name}"))?;
        ensure!(
            hashed.read == size,
            "{name} shrank from {size} to {} bytes while it was read",
            hashed.read
        );
        self.files.push(File {
            path: name,
            size,
            blake3: hashed.hasher.finalize().to_hex().to_string(),
        });
        Ok(())
    }

    /// Writes [`MANIFEST`] and ends the stream. Returns the output, not
    /// flushed to disk, and the manifest.
    pub fn finish(mut self) -> Result<(W, Manifest)> {
        let bytes = self.files.iter().map(|f| f.size).sum();
        let manifest = Manifest {
            files: std::mem::take(&mut self.files),
            bytes,
        };
        let json = serde_json::to_vec(&manifest)?;
        self.entry(MANIFEST, json.len() as u64, &mut json.as_slice())?;
        let mut out = self.tar.into_inner()?.finish()?;
        out.flush()?;
        Ok((out, manifest))
    }

    fn entry(&mut self, name: &str, size: u64, data: &mut dyn Read) -> Result<()> {
        ensure!(
            name.len() <= MAX_NAME,
            "{name} is longer than the {MAX_NAME} bytes a backup holds in a name"
        );
        let mut h = tar::Header::new_gnu();
        h.set_path(name)?;
        h.set_entry_type(tar::EntryType::Regular);
        h.set_size(size);
        h.set_mode(FILE_MODE);
        h.set_mtime(self.mtime);
        h.set_cksum();
        self.tar.append(&h, data)?;
        Ok(())
    }
}

/// The path of a file in the data directory as the archive writes it:
/// `/`-separated normal components, each valid UTF-8.
fn relative(path: &Path) -> Result<String> {
    let mut parts = Vec::new();
    for c in path.components() {
        match c {
            Component::Normal(p) => parts.push(
                p.to_str()
                    .with_context(|| format!("{} is not UTF-8", path.display()))?,
            ),
            _ => bail!("{} is not a plain relative path", path.display()),
        }
    }
    ensure!(!parts.is_empty(), "an empty path");
    Ok(parts.join("/"))
}

/// What [`read`] hands each part of a backup to, in order.
pub trait Sink {
    /// The header, before any file. An error stops the read there.
    fn header(&mut self, header: &Header) -> Result<()>;
    /// A file at `path` in the data directory, `size` bytes in `data`.
    /// What the sink leaves unread is still read and checked.
    fn file(&mut self, path: &Path, size: u64, data: &mut dyn Read) -> Result<()>;
}

/// A sink that only reads, for `davy backup verify`.
pub struct Check;

impl Sink for Check {
    fn header(&mut self, _: &Header) -> Result<()> {
        Ok(())
    }
    fn file(&mut self, _: &Path, _: u64, _: &mut dyn Read) -> Result<()> {
        Ok(())
    }
}

/// Reads a backup from `input` into `sink` and checks it: the header comes
/// first, every file's size and hash match the manifest, nothing follows
/// the manifest, and the zstd frame's own checksum holds. Returns the
/// header and the manifest only when all of that holds. Entries are read
/// raw, so a GNU long name or a PAX header is refused as an entry that is
/// not a file before the tar crate reads it.
pub fn read(input: impl Read, sink: &mut impl Sink) -> Result<(Header, Manifest)> {
    let zstd = zstd::stream::read::Decoder::new(input).context("reading the zstd stream")?;
    let mut tar = tar::Archive::new(zstd);
    let mut entries = tar.entries().context("not a Davy backup")?.raw(true);
    let header = first(&mut entries)?;
    sink.header(&header)?;
    let mut seen: Vec<File> = Vec::new();
    let mut names = HashSet::new();
    let mut bytes = 0_u64;
    let manifest = loop {
        let mut entry = entries
            .next()
            .context("the backup ends before its manifest: it is cut short")?
            .context("reading the backup")?;
        let name = entry_name(&entry)?;
        if name == Path::new(MANIFEST) {
            break json::<Manifest>(&mut entry, manifest_cap(&seen), MANIFEST)?;
        }
        let path = data_path(&name)?;
        ensure!(
            names.insert(path.clone()),
            "{} is in the backup twice",
            path.display()
        );
        let size = entry.size();
        bytes = bytes.saturating_add(size);
        ensure!(
            (seen.len() as u64) < header.files && bytes <= header.bytes,
            "the backup holds more than the {} files of {} bytes its header gives",
            header.files,
            header.bytes
        );
        let mut hashed = Hashed::new(&mut entry);
        sink.file(&path, size, &mut hashed)
            .with_context(|| format!("restoring {}", path.display()))?;
        io::copy(&mut hashed, &mut io::sink())?;
        seen.push(File {
            path: relative(&path)?,
            size: hashed.read,
            blake3: hashed.hasher.finalize().to_hex().to_string(),
        });
    };
    ensure!(
        entries.next().is_none(),
        "the backup holds an entry after its manifest"
    );
    matches(&seen, &manifest)?;
    rest(tar.into_inner())?;
    Ok((header, manifest))
}

fn first<R: Read>(entries: &mut tar::Entries<'_, R>) -> Result<Header> {
    let mut entry = entries
        .next()
        .context("the backup is empty")?
        .context("not a Davy backup")?;
    ensure!(
        entry_name(&entry)? == Path::new(HEADER),
        "not a Davy backup: it does not start with {HEADER}"
    );
    let header: Header = json(&mut entry, MAX_HEADER, HEADER)?;
    ensure!(
        header.format == FORMAT,
        "the backup is in format {}, which Davy {} does not read: restore it with the Davy {} that took it, or a newer one",
        header.format,
        env!("CARGO_PKG_VERSION"),
        header.davy_version
    );
    Ok(header)
}

fn entry_name<R: Read>(entry: &tar::Entry<'_, R>) -> Result<PathBuf> {
    ensure!(
        entry.header().entry_type() == tar::EntryType::Regular,
        "the backup holds an entry that is not a file: {}",
        String::from_utf8_lossy(&entry.path_bytes())
    );
    Ok(entry
        .path()
        .context("reading an entry's path")?
        .into_owned())
}

/// `name` under [`DATA`], as a path in the data directory that cannot
/// leave it: only normal components, at least one. A backup holds no name
/// that starts with a dot, which is what restore keeps for itself, and
/// nothing at or under a name of [`NOT_AT_TOP`] at the top.
fn data_path(name: &Path) -> Result<PathBuf> {
    let shown = || name.display().to_string();
    let rest = name
        .strip_prefix(DATA)
        .map_err(|_outside| anyhow::anyhow!("the backup holds {}, outside {DATA}/", shown()))?;
    ensure!(
        rest.components().count() > 0
            && rest.components().all(|c| matches!(c, Component::Normal(_))),
        "the backup holds {}, which is not a plain path",
        shown()
    );
    ensure!(
        rest.components()
            .all(|c| !c.as_os_str().as_encoded_bytes().starts_with(b".")),
        "the backup holds {}, a name that starts with a dot, which no backup holds",
        shown()
    );
    ensure!(
        !rest
            .components()
            .next()
            .is_some_and(|top| NOT_AT_TOP.iter().any(|n| top.as_os_str() == *n)),
        "the backup holds {}, which no backup holds",
        shown()
    );
    Ok(rest.to_path_buf())
}

/// The most bytes the manifest of the files `seen` can take: each path
/// escaped at its worst, six bytes a byte, and the rest of its line.
fn manifest_cap(seen: &[File]) -> u64 {
    let lines: u64 = seen.iter().map(|f| 6 * f.path.len() as u64 + 128).sum();
    (1024 + lines).min(MAX_MANIFEST)
}

fn json<T: serde::de::DeserializeOwned>(data: impl Read, max: u64, name: &str) -> Result<T> {
    let mut bytes = Vec::new();
    data.take(max + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= max, "{name} is over {max} bytes");
    serde_json::from_slice(&bytes).with_context(|| format!("reading {name}"))
}

fn matches(seen: &[File], manifest: &Manifest) -> Result<()> {
    for (i, want) in manifest.files.iter().enumerate() {
        let Some(got) = seen.get(i) else {
            bail!("{} is in the manifest but not in the backup", want.path);
        };
        ensure!(
            got.path == want.path,
            "the backup holds {} where the manifest names {}",
            got.path,
            want.path
        );
        ensure!(
            got == want,
            "{} does not match the manifest: {} bytes with BLAKE3 {}, not {} bytes with {}",
            got.path,
            got.size,
            got.blake3,
            want.size,
            want.blake3
        );
    }
    if let Some(extra) = seen.get(manifest.files.len()) {
        bail!("{} is in the backup but not in the manifest", extra.path);
    }
    let bytes: u64 = seen.iter().map(|f| f.size).sum();
    ensure!(
        bytes == manifest.bytes,
        "the manifest says {} bytes, the files hold {bytes}",
        manifest.bytes
    );
    Ok(())
}

/// Reads what follows the tar stream's end: only zero padding, then the end
/// of the zstd frame, whose checksum the decoder checks there.
fn rest(mut zstd: impl Read) -> Result<()> {
    let mut buf = [0u8; 8192];
    loop {
        let n = zstd
            .read(&mut buf)
            .context("reading the end of the backup")?;
        if n == 0 {
            return Ok(());
        }
        ensure!(
            buf[..n].iter().all(|b| *b == 0),
            "the backup holds bytes after its end"
        );
    }
}

/// A reader that hashes and counts what passes through it.
struct Hashed<R> {
    inner: R,
    hasher: blake3::Hasher,
    read: u64,
}

impl<R> Hashed<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            hasher: blake3::Hasher::new(),
            read: 0,
        }
    }
}

impl<R: Read> Read for Hashed<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.hasher.update(&buf[..n]);
        self.read += n as u64;
        Ok(n)
    }
}

#[cfg(test)]
#[path = "archive_tests.rs"]
mod tests;
