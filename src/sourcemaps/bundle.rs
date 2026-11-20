//! The artifact bundle `sentry-cli` uploads: a zip with a `manifest.json`.

use super::{DebugId, MAX_FILE};
use anyhow::{Result, bail};
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

/// The source maps and minified files in a bundle that carry a debug ID.
/// Other files are skipped.
pub fn unpack(zip: &[u8]) -> Result<Vec<File>> {
    let mut archive = ZipArchive::new(Cursor::new(zip))?;
    let manifest: Manifest = serde_json::from_slice(&read(&mut archive, "manifest.json")?)?;
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
                bytes: read(&mut archive, &path)?,
            });
        }
    }
    Ok(files)
}

fn read(archive: &mut ZipArchive<Cursor<&[u8]>>, path: &str) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    archive
        .by_name(path)?
        .take(MAX_FILE as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_FILE {
        bail!("{path} is larger than 256 MiB");
    }
    Ok(bytes)
}
