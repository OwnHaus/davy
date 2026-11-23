//! Terminal escape sequences in log text. Many programs colour their output
//! even when it goes to a file or the journal. A complete CSI or OSC
//! sequence carries nothing a reader of logs needs, and it splits the word
//! after it from search, so ingest removes it. A cut or unknown sequence
//! stays, and the UI shows its control characters escaped.

use serde_json::{Map, Value};

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;

/// Removes every complete CSI sequence (`ESC [`, parameters, intermediates,
/// a final byte) and OSC sequence (`ESC ]` up to `BEL` or `ESC \`) from `s`.
pub fn strip(s: &mut String) {
    if !s.contains(char::from(ESC)) {
        return;
    }
    let b = s.as_bytes();
    let mut kept = String::with_capacity(s.len());
    let (mut copied, mut i) = (0, 0);
    while i < b.len() {
        let end = match (b[i], b.get(i + 1)) {
            (ESC, Some(b'[')) => csi_end(b, i + 2),
            (ESC, Some(b']')) => osc_end(b, i + 2),
            _ => None,
        };
        if let Some(end) = end {
            kept.push_str(&s[copied..i]);
            copied = end;
            i = end;
        } else {
            i += 1;
        }
    }
    kept.push_str(&s[copied..]);
    *s = kept;
}

/// [`strip`] on every string in `attributes`, at any depth.
pub fn strip_values(attributes: &mut Map<String, Value>) {
    attributes.values_mut().for_each(strip_value);
}

/// [`strip`] on every string in `v`, at any depth.
pub fn strip_value(v: &mut Value) {
    match v {
        Value::String(s) => strip(s),
        Value::Array(items) => items.iter_mut().for_each(strip_value),
        Value::Object(m) => strip_values(m),
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

/// The end of a CSI sequence whose parameters start at `i`, after its final
/// byte, or `None` when a byte that cannot be in one comes first.
fn csi_end(b: &[u8], mut i: usize) -> Option<usize> {
    while b.get(i).is_some_and(|c| (0x30..=0x3f).contains(c)) {
        i += 1;
    }
    while b.get(i).is_some_and(|c| (0x20..=0x2f).contains(c)) {
        i += 1;
    }
    b.get(i)
        .is_some_and(|c| (0x40..=0x7e).contains(c))
        .then_some(i + 1)
}

/// The end of an OSC sequence whose text starts at `i`, after its
/// terminator, or `None` when another control character comes first. It
/// stops at the next `ESC`, so a text with many cut sequences is read once.
fn osc_end(b: &[u8], mut i: usize) -> Option<usize> {
    while let Some(&c) = b.get(i) {
        match c {
            BEL => return Some(i + 1),
            ESC => return (b.get(i + 1) == Some(&b'\\')).then_some(i + 2),
            0x00..=0x1f | 0x7f => return None,
            _ => i += 1,
        }
    }
    None
}

#[cfg(test)]
#[path = "escapes_tests.rs"]
mod tests;
