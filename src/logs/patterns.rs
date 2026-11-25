//! Log patterns: the shape of a message with its variable parts masked, so
//! lines that differ only in numbers, ids, addresses or quoted values share
//! one template and one id. Pure: text in, pattern out. Ingest runs it on the
//! scrubbed message, so a template never holds what scrubbing removed.

use std::net::Ipv6Addr;

/// The id of the pattern that counts what is over the bound of a project and
/// day. Never the id of a template.
pub const OTHER: u64 = 0;
/// Tokens of a message that make its template. The rest of a line is mostly
/// values, and a longer prefix splits one kind of line into many patterns.
pub const TOKENS: usize = 12;
/// Bytes of a message the masker reads, so a 64 KiB line costs what a short
/// one does.
const SCAN: usize = 4096;
/// Bytes a masked token keeps. A longer token is cut and ends in `...`.
const TOKEN_BYTES: usize = 64;
/// The version of the mask. An id is defined per mask version: the same
/// template has another id under another version. Any change to the masking,
/// or to the parsing of times and addresses it depends on, bumps this.
/// `patterns_v1.golden` pins mask 1 with a line for every branch a mutation
/// run found a message for, so most such changes fail its test, not all: a
/// change also adds the golden line that shows it.
pub const MASK: u8 = 1;
/// Hashed in front of the template: `v1\0` for mask 1, the bytes ids were
/// made with before the version had a number.
const PREFIX: [u8; 3] = {
    assert!(MASK < 10, "the prefix holds one decimal digit");
    [b'v', b'0' + MASK, 0]
};
/// String values of these JSON keys are prose and keep their words.
const PROSE_KEYS: [&str; 5] = ["msg", "message", "error", "err", "event"];
const METHODS: [&str; 9] = [
    "GET", "POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS", "CONNECT", "TRACE",
];
const UNITS: [&str; 17] = [
    "ns", "us", "ms", "s", "m", "h", "d", "b", "k", "kb", "mb", "gb", "tb", "kib", "mib", "gib",
    "tib",
];
/// What the scrubber puts in place of a value, minus the closing bracket.
const FILTERED: &str = "[Filtered: ";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    /// 48 bits of the hash of the template, never [`OTHER`].
    pub id: u64,
    pub template: String,
}

pub fn of(message: &str) -> Pattern {
    let template = template(message);
    Pattern {
        id: id(&template),
        template,
    }
}

/// The first [`TOKENS`] tokens of `message`, masked and joined by one space.
pub fn template(message: &str) -> String {
    let text = &message[..message.floor_char_boundary(SCAN)];
    let mut out = String::with_capacity(text.len().min(256));
    let mut token = String::new();
    for (n, pieces) in tokens(text).iter().enumerate() {
        if n > 0 {
            out.push(' ');
        }
        token.clear();
        mask(pieces, &mut token);
        if token.len() > TOKEN_BYTES {
            token.truncate(token.floor_char_boundary(TOKEN_BYTES));
            token.push_str("...");
        }
        out.push_str(&token);
    }
    out
}

pub fn id(template: &str) -> u64 {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&PREFIX);
    hasher.update(template.as_bytes());
    let mut bytes = [0; 8];
    bytes[2..].copy_from_slice(&hasher.finalize().as_bytes()[..6]);
    u64::from_be_bytes(bytes).max(1)
}

/// The id as tools print it: 12 hex digits, or `other`.
pub fn show(id: u64) -> String {
    if id == OTHER {
        "other".into()
    } else {
        format!("{id:012x}")
    }
}

/// The inverse of [`show`].
pub fn parse(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if s == "other" {
        return Ok(OTHER);
    }
    let bad = || format!("pattern {s:?} is not 12 hex digits or \"other\"");
    // from_str_radix alone would take a sign.
    if s.len() != 12 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(bad());
    }
    u64::from_str_radix(s, 16).map_err(|_checked_above| bad())
}

enum Piece<'a> {
    Raw(&'a str),
    /// A quoted JSON key, or quoted prose: the words stay, digits go.
    Words(&'a str, char),
    /// A quoted value.
    Str,
    /// A scrubber marker, by the kind it names.
    Filtered(&'a str),
}

/// Splits on whitespace, but a quoted string or a scrubber marker stays in
/// one token, and inside braces a comma ends a token, so a JSON object
/// counts one token per field.
fn tokens<'a>(text: &'a str) -> Vec<Vec<Piece<'a>>> {
    let bytes = text.as_bytes();
    let mut tokens = Vec::new();
    let mut pieces = Vec::new();
    let mut raw = None;
    let mut depth = 0_usize;
    let mut key = "";
    let mut i = 0;
    let end_raw = |raw: &mut Option<usize>, pieces: &mut Vec<Piece<'a>>, at: usize| {
        if let Some(start) = raw.take() {
            pieces.push(Piece::Raw(&text[start..at]));
        }
    };
    while i < bytes.len() && tokens.len() < TOKENS {
        let b = bytes[i];
        let comma = b == b',' && depth > 0;
        if b.is_ascii_whitespace() || comma {
            if comma {
                raw.get_or_insert(i);
            }
            i += 1;
            end_raw(&mut raw, &mut pieces, if comma { i } else { i - 1 });
            if !pieces.is_empty() {
                tokens.push(std::mem::take(&mut pieces));
            }
            continue;
        }
        if let Some((piece, next)) = quoted(text, i, &mut key) {
            end_raw(&mut raw, &mut pieces, i);
            pieces.push(piece);
            i = next;
            continue;
        }
        if let Some(close) = filtered(text, i) {
            end_raw(&mut raw, &mut pieces, i);
            pieces.push(Piece::Filtered(&text[i + FILTERED.len()..close]));
            i = close + 1;
            continue;
        }
        match b {
            b'{' => depth += 1,
            b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
        raw.get_or_insert(i);
        i += 1;
    }
    end_raw(&mut raw, &mut pieces, i);
    if !pieces.is_empty() && tokens.len() < TOKENS {
        tokens.push(pieces);
    }
    tokens
}

/// The quoted string that opens at `i`, and where the text goes on after it.
/// A quote after a letter or a digit is an apostrophe, and a quote with no
/// closing one is text.
fn quoted<'a>(text: &'a str, i: usize, key: &mut &'a str) -> Option<(Piece<'a>, usize)> {
    let bytes = text.as_bytes();
    let q = bytes[i];
    if q != b'"' && q != b'\'' {
        return None;
    }
    if i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'\\') {
        return None;
    }
    let mut j = i + 1;
    while j < bytes.len() && bytes[j] != q {
        j += if bytes[j] == b'\\' { 2 } else { 1 };
    }
    if j >= bytes.len() {
        return None;
    }
    let body = &text[i + 1..j];
    let after_colon = text[..i].trim_end().ends_with(':');
    let piece = if q == b'"' && bytes.get(j + 1) == Some(&b':') {
        *key = body;
        Piece::Words(body, '"')
    } else if (after_colon && PROSE_KEYS.contains(key)) || request_line(body) {
        Piece::Words(body, char::from(q))
    } else {
        Piece::Str
    };
    Some((piece, j + 1))
}

/// The request line of an access log, which holds the route.
fn request_line(s: &str) -> bool {
    s.split_once(' ')
        .is_some_and(|(method, _)| METHODS.contains(&method))
}

fn filtered(text: &str, i: usize) -> Option<usize> {
    let rest = text.as_bytes()[i..].strip_prefix(FILTERED.as_bytes())?;
    let close = rest.iter().take(32).position(|b| *b == b']')?;
    Some(i + FILTERED.len() + close)
}

fn mask(pieces: &[Piece<'_>], out: &mut String) {
    if let [Piece::Raw(raw)] = pieces
        && whole(raw, out)
    {
        return;
    }
    for piece in pieces {
        match piece {
            Piece::Raw(s) => words(s, out),
            Piece::Words(s, q) => {
                out.push(*q);
                words(s, out);
                out.push(*q);
            }
            Piece::Str => out.push_str("<STR>"),
            Piece::Filtered("ip") => out.push_str("<IP>"),
            Piece::Filtered("email") => out.push_str("<EMAIL>"),
            Piece::Filtered(kind) => {
                out.push_str(FILTERED);
                out.push_str(kind);
                out.push(']');
            }
        }
    }
}

/// Masks a token that is one value as a whole: a UUID, an IP address, an
/// e-mail address or a timestamp, alone, in brackets, or after `key=`.
fn whole(token: &str, out: &mut String) -> bool {
    let rest = token.trim_start_matches(['(', '[', '{', '<']);
    let core = rest.trim_end_matches([')', ']', '}', '>', ',', ';', '.']);
    let (prefix, suffix) = (&token[..token.len() - rest.len()], &rest[core.len()..]);
    let (key, value) = match core.split_once('=') {
        Some((key, value)) => (Some(key), value),
        None => (None, core),
    };
    let Some(placeholder) = value_kind(value) else {
        return false;
    };
    out.push_str(prefix);
    if let Some(key) = key {
        words(key, out);
        out.push('=');
    }
    out.push_str(placeholder);
    out.push_str(suffix);
    true
}

fn value_kind(v: &str) -> Option<&'static str> {
    if uuid_at(v) == Some(v.len()) {
        return Some("<UUID>");
    }
    if ipv4_at(v) == Some(v.len()) || ipv6(v) {
        return Some("<IP>");
    }
    if email(v) {
        return Some("<EMAIL>");
    }
    let dated = v.len() >= 19 && v.as_bytes()[0].is_ascii_digit() && v.as_bytes()[10] == b'T';
    (dated && crate::time::parse(v).is_some()).then_some("<TS>")
}

fn ipv6(v: &str) -> bool {
    let v = match v.strip_prefix('[') {
        Some(rest) => rest.split_once(']').map_or(rest, |(addr, _)| addr),
        None => v,
    };
    v.bytes().filter(|b| *b == b':').count() >= 2 && v.parse::<Ipv6Addr>().is_ok()
}

fn email(v: &str) -> bool {
    let Some((local, domain)) = v.split_once('@') else {
        return false;
    };
    let local_ok = local
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"._%+-".contains(&b));
    let domain_ok = domain
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-');
    !local.is_empty() && local_ok && domain_ok && domain.contains('.') && !domain.ends_with('.')
}

/// Masks each word of `s` and keeps what is between words. A word is a run
/// of ASCII letters and digits, so `_`, `-`, `.` and `/` split words.
fn words(s: &str, out: &mut String) {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_alphanumeric() {
            let rest = &s[i..];
            if let Some(n) = uuid_at(rest) {
                out.push_str("<UUID>");
                i += n;
            } else if let Some(n) = ipv4_at(rest) {
                out.push_str("<IP>");
                i += n;
            } else {
                let n = rest.bytes().take_while(u8::is_ascii_alphanumeric).count();
                word(&rest[..n], out);
                i += n;
            }
        } else if b.is_ascii_whitespace() {
            if !out.ends_with(' ') {
                out.push(' ');
            }
            i += 1;
        } else {
            let c = s[i..].chars().next().unwrap_or(' ');
            out.push(c);
            i += c.len_utf8();
        }
    }
}

fn word(w: &str, out: &mut String) {
    let digits = w.bytes().take_while(u8::is_ascii_digit).count();
    if digits == w.len() {
        number(out);
    } else if !w.bytes().any(|b| b.is_ascii_digit()) {
        out.push_str(w);
    } else if let Some(hex) = w.strip_prefix("0x").or_else(|| w.strip_prefix("0X"))
        && hex.bytes().all(|b| b.is_ascii_hexdigit())
    {
        out.push_str("<HEX>");
    } else if w.len() >= 8 && w.bytes().all(|b| b.is_ascii_hexdigit()) {
        out.push_str("<HEX>");
    } else if digits > 0 && UNITS.contains(&w[digits..].to_ascii_lowercase().as_str()) {
        number(out);
        out.push_str(&w[digits..]);
    } else {
        out.push_str("<ID>");
    }
}

/// Pushes `<NUM>`, folding a number that is digit groups (`2024-01-15`,
/// `10:23:45.120`, `1,024`) into one, and a sign (`blk_-42`) into the number.
fn number(out: &mut String) {
    const NUM: &str = "<NUM>";
    let sep = out.chars().next_back();
    if let Some(sep @ ('.' | ':' | ',' | '/' | '-')) = sep {
        let before = &out[..out.len() - 1];
        if before.ends_with(NUM) {
            out.pop();
            return;
        }
        let word_before = before
            .chars()
            .next_back()
            .is_some_and(char::is_alphanumeric);
        if sep == '-' && !word_before {
            out.pop();
        }
    }
    out.push_str(NUM);
}

/// The length of the UUID at the start of `s`, if one is there as a word.
fn uuid_at(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    if b.len() < 36 || b.get(36).is_some_and(u8::is_ascii_alphanumeric) {
        return None;
    }
    let ok = b[..36].iter().enumerate().all(|(i, c)| match i {
        8 | 13 | 18 | 23 => *c == b'-',
        _ => c.is_ascii_hexdigit(),
    });
    ok.then_some(36)
}

/// The length of the IPv4 address at the start of `s`, with its port if one
/// follows, if one is there as a word.
fn ipv4_at(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut i = 0;
    for part in 0..4 {
        if part > 0 {
            if b.get(i) != Some(&b'.') {
                return None;
            }
            i += 1;
        }
        let n = b[i..].iter().take_while(|c| c.is_ascii_digit()).count();
        if n == 0 || n > 3 || s[i..i + n].parse::<u16>().ok()? > 255 {
            return None;
        }
        i += n;
    }
    if b.get(i) == Some(&b':') {
        let port = b[i + 1..].iter().take_while(|c| c.is_ascii_digit()).count();
        if (1..=5).contains(&port) {
            i += 1 + port;
        }
    }
    let next_is_word = b.get(i).is_some_and(u8::is_ascii_alphanumeric)
        || (b.get(i) == Some(&b'.') && b.get(i + 1).is_some_and(u8::is_ascii_digit));
    (!next_is_word).then_some(i)
}

#[cfg(test)]
#[path = "patterns_tests.rs"]
mod tests;
