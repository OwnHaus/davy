//! Estimates of the heap a parsed body takes, made before it is parsed.
//!
//! A small body can describe millions of values, and memory follows the
//! values, not the bytes: `[{},{},...]` costs over a hundred heap bytes for
//! every three bytes of input. So the server prices a body by its values
//! before it builds them, and every estimate here is an upper bound on what
//! the matching parse allocates.

use crate::logs::LogRecord;
use serde::de::{DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};
use std::fmt;

/// A value's slot in the array that holds it, three times over. A growing
/// array can have twice the room it uses, and while it grows it holds its
/// old room and its new one.
pub const VALUE: usize = 3 * size_of::<Value>();
/// A map entry's share of the B-tree nodes that hold it, three times over,
/// because a node can be under half full and the nodes above it add more.
pub const ENTRY: usize = 3 * (size_of::<String>() + size_of::<Value>());
/// The first node of a map that holds anything. A B-tree leaf has room for
/// eleven entries and is allocated whole.
pub const MAP: usize = 11 * (size_of::<String>() + size_of::<Value>()) + 16;
/// The first room of an array that holds anything, which has space for
/// four values.
pub const ARRAY: usize = 4 * size_of::<Value>();
/// A log record's slot in a growing batch, priced as [`VALUE`] is, without
/// its text and attributes.
pub const RECORD: usize = 3 * size_of::<LogRecord>();
/// Deeper input is refused, as `serde_json` refuses it.
const MAX_DEPTH: usize = 128;

/// Text of `len` bytes, rounded up to the allocator's 16-byte steps.
pub fn text(len: usize) -> usize {
    len.next_multiple_of(16)
}

/// The heap of a JSON tree, by the same prices [`json`] uses before parsing.
pub fn of_value(v: &Value) -> usize {
    VALUE + inside(v)
}

fn inside(v: &Value) -> usize {
    match v {
        Value::String(s) => text(s.len()),
        Value::Array(a) if a.is_empty() => 0,
        Value::Array(a) => ARRAY + a.iter().map(of_value).sum::<usize>(),
        Value::Object(m) => map(m),
        Value::Null | Value::Bool(_) | Value::Number(_) => 0,
    }
}

fn map(m: &Map<String, Value>) -> usize {
    if m.is_empty() {
        return 0;
    }
    MAP + m
        .iter()
        .map(|(k, v)| ENTRY + text(k.len()) + inside(v))
        .sum::<usize>()
}

/// The heap of a log record held in a batch.
pub fn of_record(r: &LogRecord) -> usize {
    let ids = [&r.service, &r.trace_id, &r.span_id];
    RECORD
        + text(r.message.len())
        + ids
            .into_iter()
            .flatten()
            .map(|s| text(s.len()))
            .sum::<usize>()
        + map(&r.attributes)
}

/// What parsing the JSON text `bytes` into a [`Value`] takes, plus
/// `per_object` for each object in it, found without building anything.
/// Errors where `serde_json` would.
pub fn json(bytes: &[u8], per_object: usize) -> Result<usize, serde_json::Error> {
    let mut total = VALUE;
    let mut de = serde_json::Deserializer::from_slice(bytes);
    Count {
        total: &mut total,
        per_object,
    }
    .deserialize(&mut de)?;
    de.end()?;
    Ok(total)
}

/// [`json`] for each non-blank line of `bytes`.
pub fn ndjson(bytes: &[u8], per_object: usize) -> Result<usize, serde_json::Error> {
    bytes
        .split(|b| *b == b'\n')
        .filter(|l| !l.iter().all(u8::is_ascii_whitespace))
        .map(|l| json(l, per_object))
        .sum()
}

struct Count<'a> {
    total: &'a mut usize,
    per_object: usize,
}

impl<'de> DeserializeSeed<'de> for Count<'_> {
    type Value = ();
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Count<'_> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("any JSON value")
    }
    fn visit_bool<E>(self, _: bool) -> Result<(), E> {
        Ok(())
    }
    fn visit_i64<E>(self, _: i64) -> Result<(), E> {
        Ok(())
    }
    fn visit_u64<E>(self, _: u64) -> Result<(), E> {
        Ok(())
    }
    fn visit_f64<E>(self, _: f64) -> Result<(), E> {
        Ok(())
    }
    fn visit_unit<E>(self) -> Result<(), E> {
        Ok(())
    }
    fn visit_str<E>(self, s: &str) -> Result<(), E> {
        *self.total += text(s.len());
        Ok(())
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        let per_object = self.per_object;
        let mut items = 0;
        while seq
            .next_element_seed(Count {
                total: &mut *self.total,
                per_object,
            })?
            .is_some()
        {
            *self.total += VALUE;
            items += 1;
        }
        if items > 0 {
            *self.total += ARRAY;
        }
        Ok(())
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        let per_object = self.per_object;
        *self.total += per_object;
        let mut entries = 0;
        while map
            .next_key_seed(Count {
                total: &mut *self.total,
                per_object,
            })?
            .is_some()
        {
            map.next_value_seed(Count {
                total: &mut *self.total,
                per_object,
            })?;
            *self.total += ENTRY;
            entries += 1;
        }
        if entries > 0 {
            *self.total += MAP;
        }
        Ok(())
    }
}

/// What parsing the MessagePack value `bytes` takes. The MessagePack tree
/// and the JSON tree it becomes are both held, and a MessagePack value is
/// larger than a JSON one, so the JSON price is paid again, scaled up.
/// `None` where the bytes are not one MessagePack value, or nest deeper
/// than 128.
pub fn msgpack(bytes: &[u8], per_object: usize) -> Option<usize> {
    let mut input = bytes;
    let tree = VALUE + msgpack_value(&mut input, per_object, 0)?;
    input.is_empty().then_some(tree + tree * MSGPACK_SCALE)
}

/// How many JSON values one MessagePack value outweighs.
const MSGPACK_SCALE: usize = size_of::<rmpv::Value>().div_ceil(size_of::<Value>());

fn take<'a>(input: &mut &'a [u8], n: usize) -> Option<&'a [u8]> {
    let (head, rest) = input.split_at_checked(n)?;
    *input = rest;
    Some(head)
}

fn length(input: &mut &[u8], bytes: usize) -> Option<usize> {
    let mut n = 0usize;
    for b in take(input, bytes)? {
        n = (n << 8) | usize::from(*b);
    }
    Some(n)
}

/// The heap inside one MessagePack value, by the JSON prices. A map key
/// that is not a string becomes its printed form, which takes at most eight
/// bytes for each byte of its encoding.
fn msgpack_value(input: &mut &[u8], per_object: usize, depth: usize) -> Option<usize> {
    if depth > MAX_DEPTH {
        return None;
    }
    let marker = take(input, 1)?[0];
    let (fixed, text_len, items, pairs) = match marker {
        0x00..=0x7f | 0xc0 | 0xc2 | 0xc3 | 0xe0..=0xff => (0, None, 0, 0),
        0xcc | 0xd0 => (1, None, 0, 0),
        0xcd | 0xd1 | 0xd4 => (2, None, 0, 0),
        0xca | 0xce | 0xd2 => (4, None, 0, 0),
        0xcb | 0xcf | 0xd3 => (8, None, 0, 0),
        0xd5 => (3, None, 0, 0),
        0xd6 => (5, None, 0, 0),
        0xd7 => (9, None, 0, 0),
        0xd8 => (17, None, 0, 0),
        0xa0..=0xbf => (0, Some(usize::from(marker & 0x1f)), 0, 0),
        0xd9 | 0xc4 => (0, Some(length(input, 1)?), 0, 0),
        0xda | 0xc5 => (0, Some(length(input, 2)?), 0, 0),
        0xdb | 0xc6 => (0, Some(length(input, 4)?), 0, 0),
        0xc7 => (length(input, 1)? + 1, None, 0, 0),
        0xc8 => (length(input, 2)? + 1, None, 0, 0),
        0xc9 => (length(input, 4)? + 1, None, 0, 0),
        0x90..=0x9f => (0, None, usize::from(marker & 0x0f), 0),
        0xdc => (0, None, length(input, 2)?, 0),
        0xdd => (0, None, length(input, 4)?, 0),
        0x80..=0x8f => (0, None, 0, usize::from(marker & 0x0f)),
        0xde => (0, None, 0, length(input, 2)?),
        0xdf => (0, None, 0, length(input, 4)?),
        0xc1 => return None,
    };
    take(input, fixed)?;
    let mut total = 0;
    if matches!(marker, 0xc7..=0xc9 | 0xd4..=0xd8) {
        // An extension keeps its data, though only a timestamp survives
        // into JSON.
        total += text(fixed);
    }
    if let Some(n) = text_len {
        take(input, n)?;
        // Binary becomes base64 text, a third longer.
        total += text(2 * n);
    }
    if items > 0 {
        total += ARRAY;
    }
    for _ in 0..items {
        total += VALUE + msgpack_value(input, per_object, depth + 1)?;
    }
    if marker == 0xde || marker == 0xdf || (0x80..=0x8f).contains(&marker) {
        total += per_object;
        if pairs > 0 {
            total += MAP;
        }
    }
    for _ in 0..pairs {
        let before = input.len();
        let key = msgpack_value(input, per_object, depth + 1)?;
        let printed = text(8 * (before - input.len()));
        total += ENTRY + key.max(printed) + msgpack_value(input, per_object, depth + 1)?;
    }
    Some(total)
}

#[cfg(test)]
#[path = "heap_tests.rs"]
mod tests;
