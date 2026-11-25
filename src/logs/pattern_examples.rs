//! Which pattern examples the text of a log search finds, for the time a
//! search last asked for a pattern (`docs/design.md`, "Last queried"). The
//! examples go into an index in memory with the fields of a day index, and
//! the text is parsed as `search_logs` parses it, so a word, a phrase or
//! `AND` means the same. An example is the message alone: a search for an
//! attribute value finds none.

use super::super::{Fields, schema};
use anyhow::Result;
use tantivy::{
    Index, IndexWriter, Searcher, TantivyDocument, collector::DocSetCollector, doc,
    query::QueryParser, schema::Value as _,
};

/// Examples an index holds at most, the rest left out.
pub const MAX_EXAMPLES: usize = 20;
/// The smallest heap tantivy gives a writer.
const HEAP: usize = 15_000_000;

pub struct Examples {
    index: Index,
    searcher: Searcher,
    fields: Fields,
}

impl Examples {
    /// An index of the first [`MAX_EXAMPLES`] of `examples`, each numbered
    /// by its place.
    pub fn new<'a>(examples: impl IntoIterator<Item = &'a str>) -> Result<Self> {
        let (schema, fields) = schema();
        let index = Index::create_in_ram(schema);
        let mut writer: IndexWriter = index.writer_with_num_threads(1, HEAP)?;
        for (n, text) in (0_u64..).zip(examples.into_iter().take(MAX_EXAMPLES)) {
            writer.add_document(doc!(fields.project => n, fields.message => text))?;
        }
        writer.commit()?;
        let searcher = index.reader()?.searcher();
        Ok(Self {
            index,
            searcher,
            fields,
        })
    }

    /// The places of the examples `text` finds, sorted, none when `text`
    /// is not a query.
    pub fn found(&self, text: &str) -> Vec<usize> {
        let f = &self.fields;
        let parser = QueryParser::for_index(&self.index, vec![f.message, f.values]);
        let Ok(query) = parser.parse_query(text) else {
            return Vec::new();
        };
        let Ok(docs) = self.searcher.search(&query, &DocSetCollector) else {
            return Vec::new();
        };
        let mut found: Vec<usize> = docs
            .into_iter()
            .filter_map(|address| {
                let doc: TantivyDocument = self.searcher.doc(address).ok()?;
                usize::try_from(doc.get_first(f.project)?.as_u64()?).ok()
            })
            .collect();
        found.sort_unstable();
        found
    }
}
