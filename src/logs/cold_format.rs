//! The bytes of a cold file, see "Cold tier" in `docs/design.md`.
//!
//! ```text
//! head     "DAVYCOLD", version u32
//! block*   header (25 bytes), zstd frame
//! dict     zstd dictionary, absent when the file has none
//! footer   file fields, one entry per block, blake3 of the footer's own bytes before it
//! trailer  "DAVYCOLD", version u32, footer length u32
//! ```
//!
//! Every number is little endian. A block header holds its rows, its size
//! decompressed, its oldest and newest timestamp and a bit per level it
//! holds. A footer entry holds a block's offset, length, checksum and the
//! same header, so a reader that has the footer can fetch one block by
//! its byte range. A file is written once and never changed.
//!
//! A file can come from a disk that failed or from someone who could write
//! to the data directory, so every length is checked against the file and
//! the caps below before it is used, and a block decompresses into at most
//! the size its header claims, never more than [`MAX_BLOCK_BYTES`].

use anyhow::{Context, Result, bail, ensure};

pub(super) const MAGIC: [u8; 8] = *b"DAVYCOLD";
pub(super) const VERSION: u32 = 1;
/// Magic and version at the start of the file.
pub(super) const HEAD_LEN: usize = 12;
pub(super) const HEAD: u64 = HEAD_LEN as u64;
/// Magic, version and footer length at the end of the file.
pub(super) const TRAILER_LEN: usize = 16;
pub(super) const TRAILER: u64 = TRAILER_LEN as u64;
pub(super) const HEADER: usize = 25;
const ENTRY: usize = 8 + 4 + 8 + HEADER;
/// The fixed part of the footer before its entries.
const FOOTER_FIELDS: usize = 4 + 8 + 4 + 1 + 1 + 8 + 4 + 8 + 8 + 8 + 8 + 4;
/// The most a block may take decompressed: what one block costs a reader
/// in memory, whatever the file says.
pub(super) const MAX_BLOCK_BYTES: u32 = 16 << 20;
/// The most a block may take on disk, its header and frame: zstd's bound
/// for [`MAX_BLOCK_BYTES`] with room to spare, so reading one stored block
/// is bounded too.
const MAX_STORED: u64 = HEADER as u64 + MAX_BLOCK_BYTES as u64 + (MAX_BLOCK_BYTES as u64 >> 6);
/// The most rows a block may hold. Each row takes at least
/// [`MIN_ROW_BYTES`] decompressed, so the header's size bounds them too.
pub(super) const MAX_BLOCK_ROWS: u32 = 1 << 18;
/// A level, a source, a pattern flag and five string lengths.
const MIN_ROW_BYTES: u32 = 10;
pub(super) const MAX_FOOTER: u64 = 16 << 20;
pub(super) const MAX_DICT: u32 = 1 << 20;

/// The fields of a file as a whole.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Meta {
    /// The UTC day as `YYYYMMDD`.
    pub(super) day: u32,
    pub(super) project: u64,
    pub(super) seq: u32,
    /// Whether the day index the rows came from had the `values` field.
    pub(super) values: bool,
    /// Whether it had the `pattern` field.
    pub(super) pattern: bool,
    pub(super) level: u8,
}

/// What each block's header and footer entry say about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Header {
    pub(super) rows: u32,
    pub(super) raw: u32,
    pub(super) min_ts: i64,
    pub(super) max_ts: i64,
    /// Bit `n - 1` for each level code `n` the block holds.
    pub(super) levels: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Entry {
    pub(super) offset: u64,
    pub(super) len: u32,
    pub(super) checksum: u64,
    pub(super) header: Header,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Footer {
    pub(super) meta: Meta,
    /// Offset and length of the dictionary, length 0 for none.
    pub(super) dict: (u64, u32),
    pub(super) dict_checksum: u64,
    pub(super) rows: u64,
    pub(super) min_ts: i64,
    pub(super) max_ts: i64,
    pub(super) blocks: Vec<Entry>,
}

pub(super) fn checksum(bytes: &[u8]) -> u64 {
    let hash = blake3::hash(bytes);
    let mut first = [0; 8];
    first.copy_from_slice(&hash.as_bytes()[..8]);
    u64::from_le_bytes(first)
}

pub(super) fn head() -> [u8; HEAD_LEN] {
    let mut b = [0; HEAD_LEN];
    b[..8].copy_from_slice(&MAGIC);
    b[8..].copy_from_slice(&VERSION.to_le_bytes());
    b
}

pub(super) fn check_head(b: &[u8]) -> Result<()> {
    ensure!(b.len() == 12 && b[..8] == MAGIC, "not a cold file");
    let version = u32::from_le_bytes([b[8], b[9], b[10], b[11]]);
    ensure!(
        version == VERSION,
        "cold file version {version}, not {VERSION}"
    );
    Ok(())
}

impl Header {
    pub(super) fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.rows.to_le_bytes());
        out.extend_from_slice(&self.raw.to_le_bytes());
        out.extend_from_slice(&self.min_ts.to_le_bytes());
        out.extend_from_slice(&self.max_ts.to_le_bytes());
        out.push(self.levels);
    }

    pub(super) fn decode(r: &mut Reader<'_>) -> Result<Self> {
        let h = Self {
            rows: r.u32()?,
            raw: r.u32()?,
            min_ts: r.i64()?,
            max_ts: r.i64()?,
            levels: r.u8()?,
        };
        ensure!(
            h.rows > 0 && h.rows <= MAX_BLOCK_ROWS,
            "a block of {} rows",
            h.rows
        );
        ensure!(h.raw <= MAX_BLOCK_BYTES, "a block of {} bytes", h.raw);
        ensure!(
            u64::from(h.rows) * u64::from(MIN_ROW_BYTES) <= u64::from(h.raw),
            "{} rows cannot fit in {} bytes",
            h.rows,
            h.raw
        );
        ensure!(h.min_ts <= h.max_ts, "a block that ends before it starts");
        ensure!(h.levels != 0 && h.levels < 1 << 6, "a block without levels");
        Ok(h)
    }
}

impl Footer {
    pub(super) fn encode(&self) -> Vec<u8> {
        let m = &self.meta;
        let mut out = Vec::with_capacity(FOOTER_FIELDS + ENTRY * self.blocks.len() + 8);
        out.extend_from_slice(&m.day.to_le_bytes());
        out.extend_from_slice(&m.project.to_le_bytes());
        out.extend_from_slice(&m.seq.to_le_bytes());
        out.push(u8::from(m.values) | u8::from(m.pattern) << 1);
        out.push(m.level);
        out.extend_from_slice(&self.dict.0.to_le_bytes());
        out.extend_from_slice(&self.dict.1.to_le_bytes());
        out.extend_from_slice(&self.dict_checksum.to_le_bytes());
        out.extend_from_slice(&self.rows.to_le_bytes());
        out.extend_from_slice(&self.min_ts.to_le_bytes());
        out.extend_from_slice(&self.max_ts.to_le_bytes());
        let count = u32::try_from(self.blocks.len()).unwrap_or(u32::MAX);
        out.extend_from_slice(&count.to_le_bytes());
        for e in &self.blocks {
            out.extend_from_slice(&e.offset.to_le_bytes());
            out.extend_from_slice(&e.len.to_le_bytes());
            out.extend_from_slice(&e.checksum.to_le_bytes());
            e.header.encode(&mut out);
        }
        out.extend_from_slice(&checksum(&out).to_le_bytes());
        out
    }

    /// The footer in `bytes`, of a file whose blocks and dictionary end at
    /// `end`.
    pub(super) fn decode(bytes: &[u8], end: u64) -> Result<Self> {
        let (body, sum) = bytes
            .split_last_chunk::<8>()
            .context("a footer too short")?;
        ensure!(
            checksum(body) == u64::from_le_bytes(*sum),
            "the footer's checksum does not match"
        );
        let mut r = Reader::new(body);
        let day = r.u32()?;
        let project = r.u64()?;
        let seq = r.u32()?;
        let flags = r.u8()?;
        ensure!(flags < 4, "unknown footer flags {flags}");
        let meta = Meta {
            day,
            project,
            seq,
            values: flags & 1 != 0,
            pattern: flags & 2 != 0,
            level: r.u8()?,
        };
        let dict = (r.u64()?, r.u32()?);
        let f = Self {
            meta,
            dict,
            dict_checksum: r.u64()?,
            rows: r.u64()?,
            min_ts: r.i64()?,
            max_ts: r.i64()?,
            blocks: Vec::new(),
        };
        let count = usize::try_from(r.u32()?)?;
        ensure!(
            count.checked_mul(ENTRY) == Some(r.left()),
            "a footer of {count} blocks has {} bytes for them",
            r.left()
        );
        f.with_blocks(&mut r, count, end)
    }

    fn with_blocks(mut self, r: &mut Reader<'_>, count: usize, end: u64) -> Result<Self> {
        let in_file = |offset: u64, len: u64| {
            offset >= HEAD && offset.checked_add(len).is_some_and(|stop| stop <= end)
        };
        let (dict_at, dict_len) = self.dict;
        ensure!(dict_len <= MAX_DICT, "a dictionary of {dict_len} bytes");
        ensure!(
            dict_len == 0 || in_file(dict_at, dict_len.into()),
            "the dictionary lies outside the file"
        );
        let mut rows = 0u64;
        let mut next = HEAD;
        for _ in 0..count {
            let e = Entry {
                offset: r.u64()?,
                len: r.u32()?,
                checksum: r.u64()?,
                header: Header::decode(r)?,
            };
            ensure!(
                e.offset == next
                    && u64::from(e.len) > HEADER as u64
                    && u64::from(e.len) <= MAX_STORED
                    && in_file(e.offset, e.len.into()),
                "a block at {} of {} bytes lies outside its place",
                e.offset,
                e.len
            );
            next = e.offset + u64::from(e.len);
            rows += u64::from(e.header.rows);
            self.blocks.push(e);
        }
        let tail = if dict_len == 0 { end } else { dict_at };
        ensure!(
            next == tail,
            "{} bytes between the blocks and what follows",
            tail.abs_diff(next)
        );
        ensure!(
            dict_len == 0 || dict_at + u64::from(dict_len) == end,
            "bytes between the dictionary and the footer"
        );
        ensure!(
            rows == self.rows,
            "the blocks hold {rows} rows, the footer says {}",
            self.rows
        );
        Ok(self)
    }
}

/// Reads numbers off a byte slice, failing past its end.
pub(super) struct Reader<'a> {
    bytes: &'a [u8],
}

impl<'a> Reader<'a> {
    pub(super) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }

    pub(super) fn left(&self) -> usize {
        self.bytes.len()
    }

    pub(super) fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let Some((first, rest)) = self.bytes.split_at_checked(n) else {
            bail!("{n} bytes wanted, {} left", self.bytes.len());
        };
        self.bytes = rest;
        Ok(first)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        let mut a = [0; N];
        a.copy_from_slice(self.take(N)?);
        Ok(a)
    }

    pub(super) fn u8(&mut self) -> Result<u8> {
        Ok(self.array::<1>()?[0])
    }

    pub(super) fn u32(&mut self) -> Result<u32> {
        self.array().map(u32::from_le_bytes)
    }

    pub(super) fn u64(&mut self) -> Result<u64> {
        self.array().map(u64::from_le_bytes)
    }

    pub(super) fn i64(&mut self) -> Result<i64> {
        self.array().map(i64::from_le_bytes)
    }

    /// An unsigned LEB128 number of at most ten bytes.
    pub(super) fn varint(&mut self) -> Result<u64> {
        let mut value = 0u64;
        for shift in (0..70).step_by(7) {
            let b = self.u8()?;
            let bits = u64::from(b & 0x7f);
            ensure!(shift < 63 || bits <= 1, "a varint past 64 bits");
            value |= bits << shift;
            if b & 0x80 == 0 {
                return Ok(value);
            }
        }
        bail!("a varint past 64 bits")
    }
}

pub(super) fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push(u8::try_from(v & 0x7f).unwrap_or(0) | 0x80);
        v >>= 7;
    }
    out.push(u8::try_from(v).unwrap_or(0));
}

pub(super) fn zigzag(v: i64) -> u64 {
    (v << 1 ^ v >> 63).cast_unsigned()
}

pub(super) fn unzigzag(v: u64) -> i64 {
    (v >> 1).cast_signed() ^ -(v & 1).cast_signed()
}

#[cfg(test)]
#[path = "cold_format_tests.rs"]
mod tests;
