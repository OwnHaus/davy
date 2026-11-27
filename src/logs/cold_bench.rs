//! Hooks for `examples/cold_bench.rs`, which converts a day with given
//! settings and scans the files, timed by itself, and for the fuzz target
//! `cold_file`. Not an API.

use super::{
    convert::{Settings, day},
    file::ColdFile,
    query::Matcher,
};
use crate::logs::store::LogQuery;
use anyhow::Result;
use jiff::civil::Date;
use std::path::{Path, PathBuf};

/// What one conversion wrote.
pub struct Written {
    pub files: Vec<PathBuf>,
    pub rows: u64,
    pub bytes: u64,
    /// The size of the blocks decompressed.
    pub raw: u64,
    pub blocks: usize,
}

/// Converts the day index at `hot` into files in `out`.
pub fn convert(
    hot: &Path,
    day_: Date,
    out: &Path,
    level: i32,
    block_bytes: usize,
    dict: bool,
) -> Result<Written> {
    let settings = Settings {
        level,
        block_bytes,
        dict,
    };
    let name = |project: u64| out.join(format!("{project}-1.dlc"));
    let done = day(hot, day_, 1, settings, &name, &|| false)?;
    let mut w = Written {
        files: Vec::new(),
        rows: 0,
        bytes: 0,
        raw: 0,
        blocks: 0,
    };
    for c in done {
        let f = ColdFile::open(&c.path)?;
        w.raw += f
            .footer
            .blocks
            .iter()
            .map(|b| u64::from(b.header.raw))
            .sum::<u64>();
        w.blocks += f.footer.blocks.len();
        w.rows += c.rows;
        w.bytes += c.bytes;
        w.files.push(c.path);
    }
    Ok(w)
}

/// Reads every block of `files` and counts the rows `text` matches, all
/// rows without it. Returns the matches and the rows read.
pub fn scan(files: &[PathBuf], text: Option<&str>) -> Result<(u64, u64)> {
    let mut found = 0;
    let mut rows = 0;
    for path in files {
        let mut f = ColdFile::open(path)?;
        let m = &f.footer.meta;
        let q = LogQuery {
            project: None,
            since: i64::MIN,
            until: i64::MAX,
            min_level: None,
            text: text.map(str::to_owned),
            pattern: None,
            limit: 0,
            skip: 0,
        };
        let matcher = Matcher::new(&q, m.values, m.pattern)?;
        let project = m.project;
        for i in 0..f.footer.blocks.len() {
            let block = f.block(i)?;
            rows += block.len() as u64;
            found += matcher.rows(project, &block)?.len() as u64;
        }
    }
    Ok((found, rows))
}

/// Reads `bytes` as a cold file: its footer, then every block, each
/// checked, decompressed and matched by `text`, all rows without it. A
/// block that fails is skipped, as a search skips it. Returns the rows
/// matched. Whatever the bytes, the memory it takes is bounded by the
/// format's limits.
pub fn read(bytes: &[u8], text: Option<&str>) -> Result<u64> {
    let mut f = ColdFile::read(bytes)?;
    let m = &f.footer.meta;
    let q = LogQuery {
        project: None,
        since: i64::MIN,
        until: i64::MAX,
        min_level: None,
        text: text.map(str::to_owned),
        pattern: None,
        limit: 0,
        skip: 0,
    };
    let matcher = Matcher::new(&q, m.values, m.pattern)?;
    let project = m.project;
    let mut found = 0;
    for i in 0..f.footer.blocks.len() {
        if let Ok(block) = f.block(i) {
            found += matcher.rows(project, &block)?.len() as u64;
        }
    }
    Ok(found)
}
