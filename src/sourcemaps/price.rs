//! What a build takes on the heap, found from its files before they are
//! parsed. A map of a few megabytes can describe millions of tokens and
//! lines, and the memory follows those, not the bytes.

use crate::heap;
use serde::Deserialize;
use serde::de::IgnoredAny;
use sourcemap::RawToken;
use std::{io, ops::Add};

/// An upper bound on what a build holds, in bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Price {
    /// The most it holds at once, from reading its files to indexing every
    /// line they have.
    pub parse: usize,
    /// What it keeps once parsed, with every line indexed.
    pub kept: usize,
}

impl Add for Price {
    type Output = Self;
    fn add(self, other: Self) -> Self {
        Self {
            parse: self.parse + other.parse,
            kept: self.kept + other.kept,
        }
    }
}

/// A line's entry in a file's index of line ends, three times over. The
/// index grows by doubling, so it can have twice the room it uses, and
/// while it grows it holds its old room and its new one.
const LINE: usize = 3 * size_of::<u64>();
/// A file's index starts with room for four lines.
const INDEX: usize = 4 * size_of::<u64>();

/// The price of a source map. The parse holds the decompressed map, which
/// grows by doubling, the JSON tree serde builds, the copies it turns that
/// tree into, a token for every `,` and `;` and a bit vector for range
/// mappings. Index and Hermes maps are refused, because Davy reads neither.
pub fn of_map(map: &[u8]) -> io::Result<Price> {
    let json = without_header(map);
    let Kinds {
        sections,
        x_facebook_sources,
    } = serde_json::from_slice(json).map_err(io::Error::other)?;
    if sections.is_some() || x_facebook_sources.is_some() {
        return Err(io::Error::other(
            "index and Hermes source maps are not supported",
        ));
    }
    let tree = heap::json(json, 0).map_err(io::Error::other)?;
    // `sourcemap` reserves 10 tokens beyond its separators.
    let tokens =
        (json.iter().filter(|b| matches!(b, b',' | b';')).count() + 10) * size_of::<RawToken>();
    let lines = escaped_line_ends(json) * LINE;
    Ok(Price {
        parse: 4 * map.len() + 2 * tree + tokens + lines,
        kept: tree + tokens + lines,
    })
}

/// The price of a minified file. The parse holds it twice, decompressed
/// with room to spare and copied into the shared text the build keeps.
pub fn of_minified(js: &[u8]) -> Price {
    let lines = INDEX + js.iter().filter(|b| matches!(b, b'\n' | b'\r')).count() * LINE;
    let kept = heap::text(js.len() + 16) + lines;
    Price {
        parse: 3 * js.len() + kept,
        kept,
    }
}

/// The top-level fields that make a map an index or a Hermes map.
#[derive(Deserialize)]
struct Kinds {
    sections: Option<IgnoredAny>,
    x_facebook_sources: Option<IgnoredAny>,
}

/// A map without the `)]}'` line some servers put before it, which
/// `sourcemap` skips.
fn without_header(map: &[u8]) -> &[u8] {
    if !map.first().is_some_and(|b| b")]}'".contains(b)) {
        return map;
    }
    match map.iter().position(|b| *b == b'\n') {
        Some(end) => &map[end..],
        None => &[],
    }
}

/// The line ends in the strings of JSON text. Each is an escape, `\n`, `\r`,
/// `\u000a` or `\u000d`, because JSON strings cannot hold them raw.
pub(super) fn escaped_line_ends(json: &[u8]) -> usize {
    let mut ends = 0;
    let mut rest = json;
    while let Some(at) = rest.iter().position(|b| *b == b'\\') {
        let escape = &rest[at + 1..];
        let unicode = |hex: &[u8]| escape.get(..5).is_some_and(|e| e.eq_ignore_ascii_case(hex));
        if matches!(escape.first(), Some(b'n' | b'r')) || unicode(b"u000a") || unicode(b"u000d") {
            ends += 1;
        }
        rest = escape.get(1..).unwrap_or_default();
    }
    ends
}
