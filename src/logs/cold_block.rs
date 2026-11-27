//! The rows of one cold block, column by column, before compression.
//!
//! ```text
//! ts        varint zigzag, the first absolute, then the change from the one before
//! observed  varint zigzag of observed minus ts
//! level     one byte each, the code of `LogLevel`
//! source    one byte each, the code of `LogSource`
//! pattern   one byte each, 1 when the row has a pattern id
//! ids       u64 for each row with a pattern id
//! lengths   varint each, for service, trace id, span id, message and
//!           attributes in turn; for the three optional strings 0 is none
//!           and n + 1 a string of n bytes; attributes are JSON, 0 is none
//! text      the bytes of each of the five strings in the same order
//! ```
//!
//! Columns compress better than rows, as like values sit side by side,
//! and a scan reads the timestamps and levels without the text.

use super::{
    super::hit::Row,
    format::{Header, MAX_BLOCK_BYTES, MAX_BLOCK_ROWS, Reader, put_varint, unzigzag, zigzag},
};
use anyhow::{Result, bail, ensure};

pub(super) const SERVICE: usize = 0;
pub(super) const TRACE_ID: usize = 1;
pub(super) const SPAN_ID: usize = 2;
pub(super) const MESSAGE: usize = 3;
pub(super) const ATTRIBUTES: usize = 4;
const STRINGS: usize = 5;

/// Rows being gathered into one block.
#[derive(Default)]
pub(super) struct Columns {
    rows: u32,
    min_ts: i64,
    max_ts: i64,
    last_ts: i64,
    levels: u8,
    ts: Vec<u8>,
    observed: Vec<u8>,
    level: Vec<u8>,
    source: Vec<u8>,
    has_pattern: Vec<u8>,
    ids: Vec<u8>,
    lengths: [Vec<u8>; STRINGS],
    text: [Vec<u8>; STRINGS],
}

impl Columns {
    pub(super) fn rows(&self) -> u32 {
        self.rows
    }

    /// The block's size decompressed so far.
    pub(super) fn raw(&self) -> usize {
        let fixed = self.ts.len()
            + self.observed.len()
            + self.level.len()
            + self.source.len()
            + self.has_pattern.len()
            + self.ids.len();
        fixed
            + self.lengths.iter().map(Vec::len).sum::<usize>()
            + self.text.iter().map(Vec::len).sum::<usize>()
    }

    /// Whether `row` fits after the rows already here.
    pub(super) fn fits(&self, row: &Row) -> bool {
        let text = row.message.len()
            + row.attributes.len()
            + [&row.service, &row.trace_id, &row.span_id]
                .iter()
                .map(|s| s.as_ref().map_or(0, String::len))
                .sum::<usize>();
        // Each number takes at most 10 bytes as a varint.
        let most = text + 2 * 10 + 3 + 8 + STRINGS * 10;
        self.rows < MAX_BLOCK_ROWS
            && u64::try_from(self.raw() + most).is_ok_and(|n| n <= u64::from(MAX_BLOCK_BYTES))
    }

    /// Adds `row`, which must fit, see [`Columns::fits`].
    pub(super) fn push(&mut self, row: &Row) {
        if self.rows == 0 {
            self.min_ts = row.ts;
            self.max_ts = row.ts;
            put_varint(&mut self.ts, zigzag(row.ts));
        } else {
            self.min_ts = self.min_ts.min(row.ts);
            self.max_ts = self.max_ts.max(row.ts);
            put_varint(&mut self.ts, zigzag(row.ts.wrapping_sub(self.last_ts)));
        }
        self.last_ts = row.ts;
        self.rows += 1;
        put_varint(
            &mut self.observed,
            zigzag(row.observed.wrapping_sub(row.ts)),
        );
        self.level.push(row.level);
        if (1..=6).contains(&row.level) {
            self.levels |= 1 << (row.level - 1);
        }
        self.source.push(row.source);
        self.has_pattern.push(u8::from(row.pattern.is_some()));
        if let Some(id) = row.pattern {
            self.ids.extend_from_slice(&id.to_le_bytes());
        }
        let optional = [&row.service, &row.trace_id, &row.span_id];
        for (col, s) in optional.into_iter().enumerate() {
            let len = s.as_ref().map_or(0, |s| s.len() as u64 + 1);
            put_varint(&mut self.lengths[col], len);
            self.text[col].extend_from_slice(s.as_deref().unwrap_or_default().as_bytes());
        }
        for (col, s) in [(MESSAGE, &row.message), (ATTRIBUTES, &row.attributes)] {
            put_varint(&mut self.lengths[col], s.len() as u64);
            self.text[col].extend_from_slice(s.as_bytes());
        }
    }

    /// The block's header and its bytes, and empty columns for the next.
    pub(super) fn take(&mut self) -> (Header, Vec<u8>) {
        let c = std::mem::take(self);
        let mut out = Vec::with_capacity(c.raw());
        for part in [
            &c.ts,
            &c.observed,
            &c.level,
            &c.source,
            &c.has_pattern,
            &c.ids,
        ] {
            out.extend_from_slice(part);
        }
        for part in c.lengths.iter().chain(&c.text) {
            out.extend_from_slice(part);
        }
        let header = Header {
            rows: c.rows,
            raw: u32::try_from(out.len()).unwrap_or(u32::MAX),
            min_ts: c.min_ts,
            max_ts: c.max_ts,
            // A level out of range is shown as info, see `LogLevel::from_code`.
            levels: if c.levels == 0 { 1 << 2 } else { c.levels },
        };
        (header, out)
    }
}

/// One decompressed block. The strings stay in `bytes` and are read where
/// they lie.
pub(super) struct Block {
    pub(super) ts: Vec<i64>,
    pub(super) observed: Vec<i64>,
    pub(super) level: Vec<u8>,
    pub(super) source: Vec<u8>,
    pub(super) pattern: Vec<Option<u64>>,
    /// For each string column, where each row's string starts in `bytes`,
    /// with one more for where the last ends.
    starts: [Vec<usize>; STRINGS],
    /// For each optional string, whether each row has one.
    present: [Vec<bool>; 3],
    bytes: Vec<u8>,
}

impl Block {
    /// The block of `rows` rows in `bytes`. Fails on bytes that do not
    /// hold exactly that, whatever they hold.
    pub(super) fn decode(bytes: Vec<u8>, rows: u32) -> Result<Self> {
        let n = usize::try_from(rows)?;
        let mut r = Reader::new(&bytes);
        let mut ts = Vec::with_capacity(n);
        let mut last = 0i64;
        for i in 0..n {
            let v = unzigzag(r.varint()?);
            last = if i == 0 { v } else { last.wrapping_add(v) };
            ts.push(last);
        }
        let mut observed = Vec::with_capacity(n);
        for t in &ts {
            observed.push(t.wrapping_add(unzigzag(r.varint()?)));
        }
        let level = r.take(n)?.to_vec();
        let source = r.take(n)?.to_vec();
        let flags = r.take(n)?;
        let mut pattern = Vec::with_capacity(n);
        for &flag in flags {
            pattern.push(match flag {
                0 => None,
                1 => Some(r.u64()?),
                _ => bail!("a pattern flag of {flag}"),
            });
        }
        let lengths = lengths(&mut r, n)?;
        let mut at = bytes.len() - r.left();
        let mut starts: [Vec<usize>; STRINGS] = Default::default();
        let mut present: [Vec<bool>; 3] = Default::default();
        for (col, lens) in lengths.into_iter().enumerate() {
            starts[col].reserve(n + 1);
            for len in lens {
                starts[col].push(at);
                let len = if col < 3 {
                    present[col].push(len > 0);
                    len.saturating_sub(1)
                } else {
                    len
                };
                at = at
                    .checked_add(usize::try_from(len)?)
                    .filter(|end| *end <= bytes.len())
                    .ok_or_else(|| anyhow::anyhow!("a string past the end of its block"))?;
            }
            starts[col].push(at);
        }
        ensure!(
            at == bytes.len(),
            "{} bytes after the last row",
            bytes.len() - at
        );
        Ok(Self {
            ts,
            observed,
            level,
            source,
            pattern,
            starts,
            present,
            bytes,
        })
    }

    pub(super) fn len(&self) -> usize {
        self.ts.len()
    }

    /// The levels its header gives, as the writer sets them: a bit for
    /// each of levels 1 to 6 its rows hold, and info's when none is.
    pub(super) fn levels(&self) -> u8 {
        let bits = self
            .level
            .iter()
            .filter(|l| (1..=6).contains(*l))
            .fold(0, |bits, l| bits | 1 << (l - 1));
        if bits == 0 { 1 << 2 } else { bits }
    }

    /// The bytes of string column `col` of row `i`.
    pub(super) fn raw(&self, col: usize, i: usize) -> &[u8] {
        &self.bytes[self.starts[col][i]..self.starts[col][i + 1]]
    }

    /// String column `col` of row `i`, failing when it is not UTF-8.
    pub(super) fn text(&self, col: usize, i: usize) -> Result<&str> {
        Ok(std::str::from_utf8(self.raw(col, i))?)
    }

    /// Optional string column `col` of row `i`.
    pub(super) fn optional(&self, col: usize, i: usize) -> Result<Option<&str>> {
        if self.present[col][i] {
            self.text(col, i).map(Some)
        } else {
            Ok(None)
        }
    }

    pub(super) fn row(&self, i: usize) -> Result<Row> {
        let owned = |s: Option<&str>| s.map(str::to_owned);
        Ok(Row {
            ts: self.ts[i],
            observed: self.observed[i],
            level: self.level[i],
            source: self.source[i],
            pattern: self.pattern[i],
            service: owned(self.optional(SERVICE, i)?),
            trace_id: owned(self.optional(TRACE_ID, i)?),
            span_id: owned(self.optional(SPAN_ID, i)?),
            message: self.text(MESSAGE, i)?.to_owned(),
            attributes: self.text(ATTRIBUTES, i)?.to_owned(),
        })
    }
}

/// The length columns of `n` rows, each length bounded by what is left.
fn lengths(r: &mut Reader<'_>, n: usize) -> Result<[Vec<u64>; STRINGS]> {
    let mut all: [Vec<u64>; STRINGS] = Default::default();
    for lens in &mut all {
        lens.reserve(n);
        for _ in 0..n {
            let len = r.varint()?;
            ensure!(len <= r.left() as u64, "a string longer than its block");
            lens.push(len);
        }
    }
    Ok(all)
}

#[cfg(test)]
#[path = "cold_block_tests.rs"]
mod tests;
