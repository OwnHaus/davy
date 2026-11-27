//! Writes and reads one cold file, see [`super::format`].

use super::{
    super::hit::Row,
    block::{Block, Columns},
    format::{
        Entry, Footer, HEAD, HEAD_LEN, HEADER, Header, MAX_DICT, Meta, Reader, TRAILER,
        TRAILER_LEN, check_head, checksum, head,
    },
};
use anyhow::{Context, Result, ensure};
use std::{
    fs::File,
    io::{BufWriter, Write},
    os::unix::fs::{FileExt, OpenOptionsExt},
    path::Path,
};

/// How a file is compressed.
#[derive(Clone, Debug)]
pub(super) struct Params {
    pub(super) level: i32,
    /// The size decompressed past which a block is closed.
    pub(super) block_bytes: usize,
    pub(super) dict: Option<Vec<u8>>,
}

/// A cold file being written. Rows go in oldest first.
pub(super) struct Writer {
    out: BufWriter<File>,
    at: u64,
    columns: Columns,
    blocks: Vec<Entry>,
    compressor: zstd::bulk::Compressor<'static>,
    params: Params,
    rows: u64,
    min_ts: i64,
    max_ts: i64,
}

impl Writer {
    /// Starts a new file at `path`, which must not exist.
    pub(super) fn create(path: &Path, params: Params) -> Result<Self> {
        let file = File::options()
            .write(true)
            .create_new(true)
            .custom_flags(nix::libc::O_NOFOLLOW)
            .open(path)
            .with_context(|| format!("creating {}", path.display()))?;
        let mut out = BufWriter::with_capacity(1 << 20, file);
        out.write_all(&head())?;
        let compressor = match &params.dict {
            Some(d) => zstd::bulk::Compressor::with_dictionary(params.level, d)?,
            None => zstd::bulk::Compressor::new(params.level)?,
        };
        Ok(Self {
            out,
            at: HEAD,
            columns: Columns::default(),
            blocks: Vec::new(),
            compressor,
            params,
            rows: 0,
            min_ts: i64::MAX,
            max_ts: i64::MIN,
        })
    }

    pub(super) fn push(&mut self, row: &Row) -> Result<()> {
        if self.columns.rows() > 0
            && (self.columns.raw() >= self.params.block_bytes || !self.columns.fits(row))
        {
            self.flush()?;
        }
        ensure!(
            self.columns.fits(row),
            "a log of {} bytes does not fit in a block",
            row.message.len() + row.attributes.len()
        );
        self.columns.push(row);
        self.rows += 1;
        self.min_ts = self.min_ts.min(row.ts);
        self.max_ts = self.max_ts.max(row.ts);
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        let (header, raw) = self.columns.take();
        let frame = self.compressor.compress(&raw)?;
        let mut bytes = Vec::with_capacity(HEADER);
        header.encode(&mut bytes);
        self.out.write_all(&bytes)?;
        self.out.write_all(&frame)?;
        let len = u32::try_from(HEADER + frame.len()).context("a block over 4 GiB")?;
        self.blocks.push(Entry {
            offset: self.at,
            len,
            checksum: checksum(&frame),
            header,
        });
        self.at += u64::from(len);
        Ok(())
    }

    /// Writes the rest and the footer and syncs the file. Returns its size.
    pub(super) fn finish(mut self, meta: Meta) -> Result<u64> {
        ensure!(self.rows > 0, "a cold file needs a row");
        if self.columns.rows() > 0 {
            self.flush()?;
        }
        let dict = self.params.dict.take().unwrap_or_default();
        let dict_len = u32::try_from(dict.len())?;
        ensure!(dict_len <= MAX_DICT, "a dictionary of {dict_len} bytes");
        let dict_at = if dict.is_empty() { 0 } else { self.at };
        self.out.write_all(&dict)?;
        self.at += u64::from(dict_len);
        let footer = Footer {
            meta,
            dict: (dict_at, dict_len),
            dict_checksum: if dict.is_empty() { 0 } else { checksum(&dict) },
            rows: self.rows,
            min_ts: self.min_ts,
            max_ts: self.max_ts,
            blocks: std::mem::take(&mut self.blocks),
        }
        .encode();
        self.out.write_all(&footer)?;
        self.out.write_all(&head()[..8])?;
        self.out.write_all(&head()[8..])?;
        self.out
            .write_all(&u32::try_from(footer.len())?.to_le_bytes())?;
        let file = self
            .out
            .into_inner()
            .map_err(std::io::IntoInnerError::into_error)?;
        file.sync_all()?;
        Ok(self.at + footer.len() as u64 + TRAILER)
    }
}

/// Where the bytes of a cold file come from: a local file, or bytes in
/// memory. Reads go by offset, so an object store that serves byte ranges
/// can be one too.
pub(super) trait Source {
    fn size(&self) -> Result<u64>;
    fn read_at(&self, buf: &mut [u8], at: u64) -> Result<()>;
}

impl Source for File {
    fn size(&self) -> Result<u64> {
        Ok(self.metadata()?.len())
    }

    fn read_at(&self, buf: &mut [u8], at: u64) -> Result<()> {
        Ok(self.read_exact_at(buf, at)?)
    }
}

impl Source for &[u8] {
    fn size(&self) -> Result<u64> {
        Ok(self.len() as u64)
    }

    fn read_at(&self, buf: &mut [u8], at: u64) -> Result<()> {
        let at = usize::try_from(at)?;
        let bytes = at
            .checked_add(buf.len())
            .and_then(|end| self.get(at..end))
            .context("a read past the end")?;
        buf.copy_from_slice(bytes);
        Ok(())
    }
}

/// A cold file open for reading. Its footer is checked; each block is
/// checked when it is read. What it allocates is bounded by the format's
/// limits, whatever the bytes say.
pub(super) struct ColdFile<S = File> {
    source: S,
    pub(super) footer: Footer,
    decompressor: zstd::bulk::Decompressor<'static>,
}

impl ColdFile {
    /// Opens the file at `path`. Only a regular file is read: a symlink is
    /// not followed and a FIFO does not hold up the reader.
    pub(super) fn open(path: &Path) -> Result<Self> {
        let file = File::options()
            .read(true)
            .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
            .open(path)
            .with_context(|| format!("opening {}", path.display()))?;
        ensure!(file.metadata()?.is_file(), "not a regular file");
        Self::read(file)
    }
}

impl<S: Source> ColdFile<S> {
    /// Reads the head, trailer and footer of `source`.
    pub(super) fn read(source: S) -> Result<Self> {
        let len = source.size()?;
        ensure!(len >= HEAD + TRAILER, "a cold file of {len} bytes");
        let mut start = [0; HEAD_LEN];
        source.read_at(&mut start, 0)?;
        check_head(&start)?;
        let mut trailer = [0; TRAILER_LEN];
        source.read_at(&mut trailer, len - TRAILER)?;
        check_head(&trailer[..12])?;
        let footer_len = u64::from(u32::from_le_bytes([
            trailer[12],
            trailer[13],
            trailer[14],
            trailer[15],
        ]));
        ensure!(
            footer_len <= super::format::MAX_FOOTER && footer_len <= len - HEAD - TRAILER,
            "a footer of {footer_len} bytes in a file of {len}"
        );
        let end = len - TRAILER - footer_len;
        let mut bytes = vec![0; usize::try_from(footer_len)?];
        source.read_at(&mut bytes, end)?;
        let footer = Footer::decode(&bytes, end)?;
        let decompressor = if footer.dict.1 == 0 {
            zstd::bulk::Decompressor::new()?
        } else {
            let mut dict = vec![0; usize::try_from(footer.dict.1)?];
            source.read_at(&mut dict, footer.dict.0)?;
            ensure!(
                checksum(&dict) == footer.dict_checksum,
                "the dictionary's checksum does not match"
            );
            zstd::bulk::Decompressor::with_dictionary(&dict)?
        };
        Ok(Self {
            source,
            footer,
            decompressor,
        })
    }

    /// Block `i`, read, checked and decompressed into at most the size its
    /// header claims.
    pub(super) fn block(&mut self, i: usize) -> Result<Block> {
        let e = self.footer.blocks[i];
        let bytes = self.checked(i)?;
        let raw = self
            .decompressor
            .decompress(&bytes[HEADER..], usize::try_from(e.header.raw)?)
            .with_context(|| format!("decompressing block {i}"))?;
        ensure!(
            raw.len() == usize::try_from(e.header.raw)?,
            "block {i} holds {} bytes, its header says {}",
            raw.len(),
            e.header.raw
        );
        let block = Block::decode(raw, e.header.rows).with_context(|| format!("block {i}"))?;
        let span = e.header.min_ts..=e.header.max_ts;
        ensure!(
            block.ts.iter().all(|ts| span.contains(ts)),
            "block {i} holds a row outside the times its header gives"
        );
        // A search takes a block by its header's levels, then each row by
        // its own: both must say the same.
        ensure!(
            block.levels() == e.header.levels,
            "block {i} holds rows of levels {:#b}, its header says {:#b}",
            block.levels(),
            e.header.levels
        );
        Ok(block)
    }

    /// Fails unless the file says it is of `day` and `project`, the day
    /// and project its path gives, and its times and every block's lie
    /// within that UTC day: a file moved or copied from another day or
    /// another project's name, or a header that lies, is left out whole.
    pub(super) fn belongs(&self, day: jiff::civil::Date, project: u64) -> Result<()> {
        let meta = self.footer.meta;
        ensure!(
            meta.project == project,
            "a file of project {} under project {project}",
            meta.project
        );
        ensure!(
            meta.day == super::convert::yyyymmdd(day)?,
            "a file of day {} under {day}",
            meta.day
        );
        let start = crate::time::midnight(day);
        let end = day
            .tomorrow()
            .map_or(i64::MAX, |d| crate::time::midnight(d) - 1);
        let within = |min: i64, max: i64| start <= min && max <= end;
        ensure!(
            self.footer.rows == 0 || within(self.footer.min_ts, self.footer.max_ts),
            "the file's times lie outside {day}"
        );
        for (i, e) in self.footer.blocks.iter().enumerate() {
            ensure!(
                within(e.header.min_ts, e.header.max_ts),
                "block {i}'s times lie outside {day}"
            );
        }
        Ok(())
    }

    /// Block `i` as stored, its header and checksum checked against the
    /// footer, still compressed.
    pub(super) fn checked(&self, i: usize) -> Result<Vec<u8>> {
        let e = self.footer.blocks[i];
        let mut bytes = vec![0; usize::try_from(e.len)?];
        self.source.read_at(&mut bytes, e.offset)?;
        let (head, frame) = bytes.split_at(HEADER);
        ensure!(
            Header::decode(&mut Reader::new(head))? == e.header,
            "block {i}'s header differs from the footer"
        );
        ensure!(
            checksum(frame) == e.checksum,
            "block {i}'s checksum does not match"
        );
        Ok(bytes)
    }
}

#[cfg(test)]
#[path = "cold_file_tests.rs"]
mod tests;
