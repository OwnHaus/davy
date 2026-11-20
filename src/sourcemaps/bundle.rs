//! The artifact bundle `sentry-cli` uploads: a zip with a `manifest.json`.

use super::{DebugId, MAX_FILE};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    io::{Cursor, Read},
};
use zip::ZipArchive;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Map,
    Minified,
}

#[derive(Debug)]
pub struct File {
    pub debug_id: DebugId,
    pub kind: Kind,
    pub bytes: Vec<u8>,
}

#[derive(Deserialize)]
struct Manifest {
    files: BTreeMap<String, Entry>,
}

#[derive(Deserialize)]
struct Entry {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    headers: BTreeMap<String, String>,
}

/// How much `unpack` reads from one bundle before it gives up.
#[derive(Clone, Copy, Debug)]
pub struct Limit {
    pub files: usize,
    pub bytes: usize,
}

pub const BUNDLE: Limit = Limit {
    files: 10_000,
    bytes: 1 << 30,
};

/// The source maps and minified files in a bundle that carry a debug ID.
/// Other files are skipped.
pub fn unpack(zip: &[u8], limit: Limit) -> Result<Vec<File>> {
    let mut archive = ZipArchive::new(Cursor::new(zip))?;
    let mut left = limit.bytes;
    let manifest: Manifest =
        serde_json::from_slice(&read(&mut archive, "manifest.json", &mut left, limit)?)?;
    if manifest.files.len() > limit.files {
        bail!("bundle lists more than {} files", limit.files);
    }
    let mut files = Vec::new();
    for (path, entry) in manifest.files {
        let kind = match entry.kind.as_str() {
            "source_map" => Kind::Map,
            "minified_source" => Kind::Minified,
            _ => continue,
        };
        let debug_id = entry
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("debug-id"))
            .and_then(|(_, value)| DebugId::parse(value));
        if let Some(debug_id) = debug_id {
            files.push(File {
                debug_id,
                kind,
                bytes: read(&mut archive, &path, &mut left, limit)?,
            });
        }
    }
    Ok(files)
}

/// Reads `path` and takes its size from `left`, the bytes the bundle may
/// still unpack to.
fn read(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    path: &str,
    left: &mut usize,
    limit: Limit,
) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    archive
        .by_name(path)?
        .take(MAX_FILE.min(*left) as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_FILE {
        bail!("{path} is larger than 256 MiB");
    }
    *left = left
        .checked_sub(bytes.len())
        .with_context(|| format!("bundle unpacks to more than {} bytes", limit.bytes))?;
    Ok(bytes)
}
