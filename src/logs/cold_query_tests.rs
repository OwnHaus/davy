use super::{fold, required};
use crate::logs::store::variant;
use tantivy::{Index, query::QueryParser};

/// The word lists `text` needs, as a day with values parses it.
fn needs(text: &str) -> Vec<Vec<String>> {
    let (schema, f) = variant(true, true);
    let index = Index::create_in_ram(schema);
    let parser = QueryParser::for_index(&index, vec![f.message, f.values]);
    let q = parser.parse_query(text).unwrap();
    required(q.as_ref(), &[f.message, f.values, f.attributes])
}

fn words(lists: &[&[&str]]) -> Vec<Vec<String>> {
    lists
        .iter()
        .map(|l| l.iter().map(|w| (*w).to_owned()).collect())
        .collect()
}

#[test]
fn a_query_needs_the_words_it_cannot_match_without() {
    assert_eq!(needs("Timeout"), words(&[&["timeout", "timeout"]]));
    assert_eq!(needs("message:timeout"), words(&[&["timeout"]]));
    assert_eq!(needs("message:a AND message:b"), words(&[&["a"], &["b"]]));
    assert_eq!(needs("message:a OR message:b"), words(&[&["a", "b"]]));
    assert_eq!(
        needs("message:\"connection reset\""),
        words(&[&["connection"], &["reset"]])
    );
    assert_eq!(needs("attributes.user:Alice"), words(&[&["alice"]]));
    assert_eq!(needs("message:a -message:b"), words(&[&["a"]]));
    // A phrase in both default fields needs its longest word from either.
    assert_eq!(
        needs("\"blk_-1608999687\""),
        words(&[&["1608999687", "1608999687"]])
    );
    // The parser drops the star: the query is the word.
    assert_eq!(needs("message:time*"), words(&[&["time"]]));
}

#[test]
fn a_query_that_needs_no_word_has_no_list() {
    assert_eq!(needs("*"), Vec::<Vec<String>>::new());
    assert_eq!(needs("attributes.n:5"), Vec::<Vec<String>>::new());
    assert_eq!(
        needs("message:a OR attributes.n:5"),
        Vec::<Vec<String>>::new()
    );
}

#[test]
fn folding_lowercases_each_character_as_tantivy_does() {
    let mut out = Vec::new();
    assert!(fold(b"ABC def", &mut out));
    assert_eq!(out, b"abc def");
    out.clear();
    assert!(fold("ŜÜΣ İ".as_bytes(), &mut out));
    assert_eq!(String::from_utf8(out.clone()).unwrap(), "ŝüσ i\u{307}");
    out.clear();
    assert!(!fold(&[0xff], &mut out));
}

#[test]
fn the_index_of_candidates_stores_nothing_and_keeps_every_field() {
    let (day, _) = variant(true, true);
    let index = super::unstored(&day).unwrap();
    assert_eq!(index.num_fields(), 12);
    for ((field, a), (_, b)) in day.fields().zip(index.fields()) {
        assert_eq!(a.name(), b.name(), "{field:?}");
        assert_eq!(a.is_indexed(), b.is_indexed(), "{}", a.name());
        assert_eq!(a.is_fast(), b.is_fast(), "{}", a.name());
        assert!(!b.is_stored(), "{}", a.name());
    }
    assert!(
        day.get_field_entry(day.get_field("message").unwrap())
            .is_stored()
    );
}
