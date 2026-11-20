//! Source maps that `sentry-cli` uploads, stored per debug ID, and the lookup
//! symbolication uses. `upload.rs` is the HTTP side.

pub mod bundle;
mod symbolicate;
#[cfg(test)]
mod tests;

pub use symbolicate::symbolicate;

use bundle::Kind;
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use sourcemap::{SourceMap, SourceView};
use std::{
    collections::VecDeque,
    fmt, fs, io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

/// The largest bundle, and the largest file inside one, that Davy accepts.
pub const MAX_FILE: usize = 256 * 1024 * 1024;
const CACHED_BUILDS: usize = 32;

/// The UUID that `sentry-cli sourcemaps inject` writes into a minified file
/// and its map. It is derived from the content, so it names one build.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct DebugId(uuid::Uuid);

impl DebugId {
    pub fn parse(s: &str) -> Option<Self> {
        uuid::Uuid::parse_str(s).ok().map(Self)
    }
}

impl fmt::Display for DebugId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.hyphenated().fmt(f)
    }
}

/// The SHA-1 of a chunk or a whole bundle, as 40 lowercase hex digits.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct ChunkId(String);

impl ChunkId {
    pub fn of(bytes: &[u8]) -> Self {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut hex = String::with_capacity(40);
        for byte in Sha1::digest(bytes) {
            hex.push(char::from(HEX[usize::from(byte >> 4)]));
            hex.push(char::from(HEX[usize::from(byte & 0xf)]));
        }
        Self(hex)
    }
}

impl TryFrom<String> for ChunkId {
    type Error = String;
    fn try_from(s: String) -> Result<Self, String> {
        let hex = |c: char| c.is_ascii_digit() || ('a'..='f').contains(&c);
        if s.len() == 40 && s.chars().all(hex) {
            Ok(Self(s))
        } else {
            Err(format!("{s:?} is not a SHA-1"))
        }
    }
}

impl fmt::Display for ChunkId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A minified file's source map, with the minified file when it was uploaded.
/// The minified file is what recovers original function names.
pub struct Build {
    map: SourceMap,
    minified: Option<SourceView>,
}

impl Build {
    /// A build from its source map and, when it was uploaded, its minified
    /// file, which must be UTF-8.
    pub fn parse(map: &[u8], minified: Option<Vec<u8>>) -> io::Result<Self> {
        let map = SourceMap::from_slice(map).map_err(io::Error::other)?;
        let minified = match minified {
            Some(js) => Some(SourceView::new(
                String::from_utf8(js).map_err(io::Error::other)?.into(),
            )),
            None => None,
        };
        Ok(Self { map, minified })
    }
}

/// What an assemble request found.
#[derive(Debug, PartialEq, Eq)]
pub enum Assembly {
    Missing(Vec<ChunkId>),
    Created,
    Failed(String),
}

/// Uploaded chunks and stored builds under the data directory.
pub struct Store {
    chunks: PathBuf,
    builds: PathBuf,
    cache: Mutex<VecDeque<(DebugId, Arc<Build>)>>,
}

impl Store {
    pub fn open(data: &Path) -> Self {
        Self {
            chunks: data.join("chunks"),
            builds: data.join("sourcemaps"),
            cache: Mutex::default(),
        }
    }

    /// Stores a chunk whose content hashes to `id`. The caller checked that.
    pub fn put_chunk(&self, id: &ChunkId, bytes: &[u8]) -> io::Result<()> {
        write_atomic(&self.chunks.join(&id.0), bytes)
    }

    /// Joins `chunks` in order into a bundle that must hash to `checksum`,
    /// stores every map and minified file in it, then deletes the chunks.
    pub fn assemble(&self, checksum: &ChunkId, chunks: &[ChunkId]) -> io::Result<Assembly> {
        let missing: Vec<ChunkId> = chunks
            .iter()
            .filter(|c| !self.chunks.join(&c.0).exists())
            .cloned()
            .collect();
        if !missing.is_empty() {
            return Ok(Assembly::Missing(missing));
        }
        let mut bundle = Vec::new();
        for chunk in chunks {
            match fs::read(self.chunks.join(&chunk.0)) {
                Ok(bytes) => bundle.extend(bytes),
                // Another assemble of the same bundle finished first and took the chunks.
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    return Ok(Assembly::Missing(vec![chunk.clone()]));
                }
                Err(e) => return Err(e),
            }
            if bundle.len() > MAX_FILE {
                return Ok(Assembly::Failed("bundle is larger than 256 MiB".into()));
            }
        }
        let actual = ChunkId::of(&bundle);
        if actual != *checksum {
            return Ok(Assembly::Failed(format!(
                "bundle hashes to {actual}, not {checksum}"
            )));
        }
        let files = match bundle::unpack(&bundle, bundle::BUNDLE) {
            Ok(files) if files.is_empty() => {
                return Ok(Assembly::Failed(
                    "bundle has no source map or minified file with a debug ID".into(),
                ));
            }
            Ok(files) => files,
            Err(e) => return Ok(Assembly::Failed(format!("bundle does not unpack: {e:#}"))),
        };
        for file in files {
            let compressed = zstd::encode_all(file.bytes.as_slice(), 3)?;
            write_atomic(&self.path(file.debug_id, file.kind), &compressed)?;
        }
        for chunk in chunks {
            match fs::remove_file(self.chunks.join(&chunk.0)) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
        Ok(Assembly::Created)
    }

    /// The build with this debug ID, if its map was uploaded.
    pub fn build(&self, id: DebugId) -> io::Result<Option<Arc<Build>>> {
        if let Some(found) = self.cached(id) {
            return Ok(Some(found));
        }
        let Some(map) = self.read(id, Kind::Map)? else {
            return Ok(None);
        };
        let build = Arc::new(Build::parse(&map, self.read(id, Kind::Minified)?)?);
        if let Ok(mut cache) = self.cache.lock() {
            if cache.len() == CACHED_BUILDS {
                cache.pop_front();
            }
            cache.push_back((id, Arc::clone(&build)));
        }
        Ok(Some(build))
    }

    fn cached(&self, id: DebugId) -> Option<Arc<Build>> {
        let cache = self.cache.lock().ok()?;
        cache
            .iter()
            .find(|(cached, _)| *cached == id)
            .map(|(_, build)| Arc::clone(build))
    }

    fn read(&self, id: DebugId, kind: Kind) -> io::Result<Option<Vec<u8>>> {
        match fs::read(self.path(id, kind)) {
            Ok(compressed) => zstd::decode_all(compressed.as_slice()).map(Some),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn path(&self, id: DebugId, kind: Kind) -> PathBuf {
        let extension = match kind {
            Kind::Map => "map",
            Kind::Minified => "js",
        };
        self.builds.join(format!("{id}.{extension}.zst"))
    }
}

/// Writes to a temporary name and renames, so a reader never sees half a file.
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)
}
