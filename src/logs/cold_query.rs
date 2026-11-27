//! Which rows of a cold block a search matches: the same as in the day
//! index the rows came from.
//!
//! Time, project, level and pattern are compared column by column. Text
//! goes through tantivy itself: the rows that may match are indexed into
//! an index in memory with the schema of their day, and the query parsed
//! as a day parses it runs on that. So a cold row matches exactly what it
//! matched hot, for every query the language has.
//!
//! Indexing is the slow part, so first each row must contain, ignoring
//! case, every word the query cannot match without. A word of a text field
//! is a run of letters and digits, lowercased, so a row that holds it holds
//! it as text, and one that does not cannot match. Only words of `message`
//! and the attributes count, in `values` and in the JSON field, whose text
//! the row holds as written or JSON-escaped, which leaves letters and
//! digits as they were.

use super::{
    super::{
        Fields, LogQuery, document,
        hit::{Row, attributes},
        variant,
    },
    block::{ATTRIBUTES, Block, MESSAGE},
    format::Header,
};
use crate::logs::{LogLevel, LogRecord, LogSource};
use anyhow::{Result, anyhow};
use memchr::memmem::Finder;
use serde_json::Value;
use tantivy::{
    DocAddress, Index, SingleSegmentIndexWriter, TantivyDocument,
    collector::DocSetCollector,
    directory::RamDirectory,
    query::{BooleanQuery, Occur, PhraseQuery, Query, QueryParser, TermQuery, TermSetQuery},
    schema::{Field, Schema, Type},
};

/// What a cold block must match, for the files of one schema.
pub(super) struct Matcher {
    since: i64,
    until: i64,
    #[expect(dead_code, reason = "cold search reads it in the next commit")]
    project: Option<u64>,
    min_level: Option<u8>,
    pattern: Option<u64>,
    text: Option<Text>,
}

struct Text {
    index_schema: Schema,
    fields: Fields,
    values: bool,
    pattern: Option<Field>,
    query: Box<dyn Query>,
    /// Words a matching row contains: one of each list.
    needles: Vec<Vec<Finder<'static>>>,
}

/// Memory for the index of one block's candidates, tantivy's least.
const BLOCK_HEAP: usize = 15_000_000;

impl Matcher {
    /// What `q` matches in a file whose day had `values` and `pattern`.
    pub(super) fn new(q: &LogQuery, values: bool, pattern: bool) -> Result<Self> {
        let text = match q.text.as_deref().filter(|t| !t.trim().is_empty()) {
            Some(t) => Some(Text::new(t, values, pattern)?),
            None => None,
        };
        // A day without the field matches no pattern, so the window is
        // empty.
        let none = q.pattern.is_some() && !pattern;
        Ok(Self {
            since: if none { i64::MAX } else { q.since },
            until: if none { i64::MIN } else { q.until },
            project: q.project.map(i64::cast_unsigned),
            min_level: q.min_level.map(|l| l as u8),
            pattern: q.pattern,
            text,
        })
    }

    /// Whether a block of `project` from `min_ts` to `max_ts` may match.
    #[expect(dead_code, reason = "cold search reads it in the next commit")]
    pub(super) fn may_match(&self, project: u64, min_ts: i64, max_ts: i64, levels: u8) -> bool {
        let level_ok = self
            .min_level
            .is_none_or(|min| min <= 1 || levels >> (min - 1) != 0);
        self.project.is_none_or(|p| p == project)
            && max_ts >= self.since
            && min_ts <= self.until
            && level_ok
    }

    /// Whether every row of a block with header `h` matches, so the
    /// header can count them: the block lies in the window, holds no level
    /// below the least asked for, and there is no text or pattern to match.
    #[expect(dead_code, reason = "cold search reads it in the next commit")]
    pub(super) fn counts_whole(&self, h: &Header) -> bool {
        let below = |min: u8| u16::from(h.levels) & ((1u16 << min.saturating_sub(1)) - 1) != 0;
        self.text.is_none()
            && self.pattern.is_none()
            && h.min_ts >= self.since
            && h.max_ts <= self.until
            && !self.min_level.is_some_and(below)
    }

    /// The rows of `block` that match, by position, in the order they lie.
    pub(super) fn rows(&self, project: u64, block: &Block) -> Result<Vec<usize>> {
        let mut found = Vec::new();
        let mut scratch = Vec::new();
        for i in 0..block.len() {
            let ts = block.ts[i];
            if ts < self.since
                || ts > self.until
                || self.min_level.is_some_and(|min| block.level[i] < min)
                || self.pattern.is_some_and(|id| block.pattern[i] != Some(id))
            {
                continue;
            }
            if let Some(text) = &self.text
                && !text.may_hold(block, i, &mut scratch)
            {
                continue;
            }
            found.push(i);
        }
        match &self.text {
            Some(text) if !found.is_empty() => text.matching(project, block, &found),
            _ => Ok(found),
        }
    }
}

impl Text {
    fn new(text: &str, values: bool, pattern: bool) -> Result<Self> {
        let (day_schema, fields) = variant(values, pattern);
        let index = Index::create_in_ram(day_schema.clone());
        let index_schema = unstored(&day_schema)?;
        let words = if values {
            fields.values
        } else {
            fields.attributes
        };
        let parser = QueryParser::for_index(&index, vec![fields.message, words]);
        let query = parser
            .parse_query(text)
            .map_err(|e| anyhow!("invalid query {text:?}: {e}"))?;
        let text_fields = [fields.message, words, fields.attributes];
        let needles = required(query.as_ref(), &text_fields)
            .into_iter()
            .map(|any| {
                any.into_iter()
                    .map(|w| Finder::new(w.as_bytes()).into_owned())
                    .collect()
            })
            .collect();
        Ok(Self {
            pattern: day_schema.get_field("pattern").ok(),
            index_schema,
            fields,
            values,
            query,
            needles,
        })
    }

    /// Whether row `i` holds a word of each list of [`Text::needles`].
    fn may_hold(&self, block: &Block, i: usize, scratch: &mut Vec<u8>) -> bool {
        if self.needles.is_empty() {
            return true;
        }
        scratch.clear();
        for col in [MESSAGE, ATTRIBUTES] {
            if !fold(block.raw(col, i), scratch) {
                return true;
            }
            scratch.push(0);
        }
        self.needles
            .iter()
            .all(|any| any.iter().any(|f| f.find(scratch).is_some()))
    }

    /// Which of the `rows` of `block` the query matches.
    fn matching(&self, project: u64, block: &Block, rows: &[usize]) -> Result<Vec<usize>> {
        let mut writer: SingleSegmentIndexWriter<TantivyDocument> = Index::builder()
            .schema(self.index_schema.clone())
            .single_segment_index_writer(RamDirectory::create(), BLOCK_HEAP)?;
        for &i in rows {
            let record = record(project, &block.row(i)?)?;
            let pattern = self.pattern.zip(block.pattern[i]);
            writer.add_document(document(&self.fields, record, self.values, pattern))?;
        }
        let index = writer.finalize()?;
        let reader = index.reader()?;
        let hits = reader.searcher().search(&*self.query, &DocSetCollector)?;
        let mut found: Vec<usize> = hits
            .into_iter()
            .map(|DocAddress { doc_id, .. }| rows[doc_id as usize])
            .collect();
        found.sort_unstable();
        Ok(found)
    }
}

/// The record `row` of `project` was stored from, as the day index saw it.
pub(super) fn record(project: u64, row: &Row) -> Result<LogRecord> {
    Ok(LogRecord {
        project_id: project.cast_signed(),
        timestamp: row.ts,
        observed_at: row.observed,
        source: LogSource::from_code(row.source.into()),
        level: LogLevel::from_code(row.level.into()),
        level_unknown: false,
        message: row.message.clone(),
        service: row.service.clone(),
        trace_id: row.trace_id.clone(),
        span_id: row.span_id.clone(),
        attributes: attributes(&row.attributes)?,
    })
}

/// `schema` with nothing stored: the index of a block's candidates is
/// searched, never read, and storing its documents was most of its cost.
/// Field ids, types and indexing stay, so a query parsed for the day runs
/// on it as on the day.
fn unstored(schema: &Schema) -> Result<Schema> {
    let mut json = serde_json::to_value(schema)?;
    for entry in json.as_array_mut().into_iter().flatten() {
        if let Some(stored) = entry.pointer_mut("/options/stored") {
            *stored = Value::Bool(false);
        }
    }
    Ok(serde_json::from_value(json)?)
}

/// Appends `text` lowercased the way tantivy's `LowerCaser` lowercases a
/// word, one character at a time. False when `text` is not UTF-8.
fn fold(text: &[u8], out: &mut Vec<u8>) -> bool {
    if text.is_ascii() {
        out.extend(text.iter().map(u8::to_ascii_lowercase));
        return true;
    }
    let Ok(text) = std::str::from_utf8(text) else {
        return false;
    };
    let mut buf = [0; 4];
    for c in text.chars().flat_map(char::to_lowercase) {
        out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
    }
    true
}

/// Lists of words of `fields` of which a document `q` matches holds one
/// each. Empty when `q` needs no word. A query it does not know needs none.
pub(super) fn required(q: &dyn Query, fields: &[Field]) -> Vec<Vec<String>> {
    if let Some(t) = q.downcast_ref::<TermQuery>() {
        return word(t.term(), fields)
            .map(|w| vec![vec![w]])
            .unwrap_or_default();
    }
    if let Some(p) = q.downcast_ref::<PhraseQuery>() {
        return p
            .phrase_terms()
            .iter()
            .filter_map(|t| word(t, fields).map(|w| vec![w]))
            .collect();
    }
    if let Some(s) = q.downcast_ref::<TermSetQuery>() {
        let mut any = Vec::new();
        let mut all = true;
        s.query_terms(&mut |t, _| match word(t, fields) {
            Some(w) => any.push(w),
            None => all = false,
        });
        return if all && !any.is_empty() {
            vec![any]
        } else {
            Vec::new()
        };
    }
    let Some(b) = q.downcast_ref::<BooleanQuery>() else {
        return Vec::new();
    };
    let must: Vec<_> = b
        .clauses()
        .iter()
        .filter(|(o, _)| *o == Occur::Must)
        .flat_map(|(_, q)| required(q.as_ref(), fields))
        .collect();
    if b.clauses().iter().any(|(o, _)| *o == Occur::Must) {
        return must;
    }
    // Without a must, a match matches one should, so it holds a word of
    // each of that should's lists. Any one list will do; the one whose
    // shortest word is longest likely holds the rarest words, as in the
    // phrase "blk_-1608999687919862906", where every line holds blk.
    let mut any = Vec::new();
    for (o, q) in b.clauses() {
        if *o != Occur::Should {
            continue;
        }
        let lists = required(q.as_ref(), fields);
        let rarest = lists
            .into_iter()
            .max_by_key(|l| l.iter().map(String::len).min().unwrap_or(0));
        match rarest {
            Some(first) => any.extend(first),
            None => return Vec::new(),
        }
    }
    if any.is_empty() {
        Vec::new()
    } else {
        vec![any]
    }
}

/// The text of `term` when it is a word of one of `fields`.
fn word(term: &tantivy::Term, fields: &[Field]) -> Option<String> {
    if !fields.contains(&term.field()) {
        return None;
    }
    match term.typ() {
        Type::Str => term.value().as_str().map(str::to_owned),
        Type::Json if term.value().json_path_type() == Some(Type::Str) => {
            let bytes = term.serialized_value_bytes();
            let end = bytes.iter().position(|b| *b == 0)?;
            let text = bytes.get(end + 2..)?;
            std::str::from_utf8(text).ok().map(str::to_owned)
        }
        _ => None,
    }
    .filter(|w| !w.is_empty())
}

#[cfg(test)]
#[path = "cold_query_tests.rs"]
mod tests;
