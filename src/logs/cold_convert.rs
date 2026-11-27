//! Turns one hot day index into cold files, one per project.

use super::{
    super::{Fields, has_values, hit::stored, pattern_field, schema},
    file::{Params, Writer},
    format::Meta,
};
use anyhow::{Context, Result, ensure};
use jiff::civil::Date;
use std::path::{Path, PathBuf};
use tantivy::{DocAddress, Index, IndexReader, ReloadPolicy, Searcher, TantivyDocument};

/// Decompressed docstore blocks the conversion keeps per segment. Logs
/// are read by time, which is near the order they were written in.
const DOC_BLOCKS: usize = 64;
/// How many rows a conversion writes between looks at its stop flag.
const STOP_EVERY: usize = 4096;
/// The dictionary trained for a day when one is asked for.
const DICT_BYTES: usize = 112 << 10;
/// Rows sampled to train it.
const DICT_SAMPLES: usize = 20_000;

/// What the conversion of a day wrote for one project.
#[derive(Debug)]
pub(super) struct Converted {
    pub(super) path: PathBuf,
    pub(super) rows: u64,
    pub(super) bytes: u64,
}

/// How to compress, with or without a dictionary trained on the day.
#[derive(Clone, Copy, Debug)]
pub(super) struct Settings {
    pub(super) level: i32,
    pub(super) block_bytes: usize,
    pub(super) dict: bool,
}

/// Where a log lies in the day index, ordered for the files.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Key {
    project: u64,
    ts: i64,
    segment: u32,
    doc: u32,
}

/// Writes the logs of the day index at `hot` into one file per project in
/// `out`, at the path `name` gives for a project. Returns what it wrote,
/// in project order. `stop` is asked between projects and ends the work.
pub(super) fn day(
    hot: &Path,
    day: Date,
    seq: u32,
    settings: Settings,
    name: &dyn Fn(u64) -> PathBuf,
    stop: &dyn Fn() -> bool,
) -> Result<Vec<Converted>> {
    let index = Index::open_in_dir(hot).with_context(|| format!("open {}", hot.display()))?;
    let reader: IndexReader = index
        .reader_builder()
        .reload_policy(ReloadPolicy::Manual)
        .doc_store_cache_num_blocks(DOC_BLOCKS)
        .try_into()?;
    let searcher = reader.searcher();
    let keys = keys(&searcher)?;
    let (schema, fields) = (index.schema(), schema().1);
    let values = has_values(&index);
    let pattern = pattern_field(&index).is_some();
    let dict = if settings.dict {
        Some(train(&searcher, &schema, &fields, &keys)?)
    } else {
        None
    };
    let mut out = Vec::new();
    for group in keys.chunk_by(|a, b| a.project == b.project) {
        ensure!(!stop(), "the conversion of {day} was stopped");
        let project = group[0].project;
        let path = name(project);
        let params = Params {
            level: settings.level,
            block_bytes: settings.block_bytes,
            dict: dict.clone(),
        };
        let mut w = Writer::create(&path, params)?;
        for (n, k) in group.iter().enumerate() {
            // A project may hold most of the day: stop between its rows too.
            if n % STOP_EVERY == 0 {
                ensure!(!stop(), "the conversion of {day} was stopped");
            }
            let doc: TantivyDocument = searcher.doc(address(k))?;
            w.push(&stored(&schema, &fields, &doc)?.1)?;
        }
        let meta = Meta {
            day: yyyymmdd(day)?,
            project,
            seq,
            values,
            pattern,
            level: u8::try_from(settings.level).unwrap_or(0),
        };
        let bytes = w.finish(meta)?;
        let rows = group.len() as u64;
        out.push(Converted { path, rows, bytes });
    }
    Ok(out)
}

fn address(k: &Key) -> DocAddress {
    DocAddress::new(k.segment, k.doc)
}

/// Every live log of the day by project and time.
fn keys(searcher: &Searcher) -> Result<Vec<Key>> {
    let mut keys = Vec::with_capacity(usize::try_from(searcher.num_docs())?);
    for (segment, reader) in searcher.segment_readers().iter().enumerate() {
        let project = reader.fast_fields().u64("project")?;
        let ts = reader.fast_fields().i64("ts")?;
        let alive = reader.alive_bitset();
        for doc in 0..reader.max_doc() {
            if alive.is_some_and(|a| !a.is_alive(doc)) {
                continue;
            }
            keys.push(Key {
                project: project.first(doc).unwrap_or(0),
                ts: ts.first(doc).unwrap_or(0),
                segment: u32::try_from(segment)?,
                doc,
            });
        }
    }
    keys.sort_unstable();
    Ok(keys)
}

/// A zstd dictionary from the text of logs spread over the day.
fn train(
    searcher: &Searcher,
    schema: &tantivy::schema::Schema,
    fields: &Fields,
    keys: &[Key],
) -> Result<Vec<u8>> {
    let step = (keys.len() / DICT_SAMPLES).max(1);
    let mut samples = Vec::new();
    for k in keys.iter().step_by(step) {
        let doc: TantivyDocument = searcher.doc(address(k))?;
        let row = stored(schema, fields, &doc)?.1;
        samples.push(row.message.into_bytes());
        if !row.attributes.is_empty() {
            samples.push(row.attributes.into_bytes());
        }
    }
    Ok(zstd::dict::from_samples(&samples, DICT_BYTES)?)
}

/// `day` as the number `YYYYMMDD`.
pub(super) fn yyyymmdd(day: Date) -> Result<u32> {
    let n = i32::from(day.year()) * 10_000 + i32::from(day.month()) * 100 + i32::from(day.day());
    Ok(u32::try_from(n)?)
}
