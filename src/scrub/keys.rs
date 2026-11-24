//! Which keys name sensitive values, decided without allocating.
use super::Policy;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::HashSet;

/// How much of the end of a key is checked. Real keys are far shorter, and
/// a sensitive name ends in its root, so a longer key is checked by its end.
const WINDOW: usize = 256;
/// Roots distinctive enough to match anywhere in a normalized key.
const ROOTS: &[&str] = &[
    "password",
    "passwd",
    "passphrase",
    "secret",
    "credential",
    "apikey",
    "privatekey",
    "accesskey",
    "accountkey",
    "sharedaccesskey",
    "clientsecret",
    "creditcard",
    "cardnumber",
    "csrf",
    "xsrf",
    "cookie",
    "authorization",
    "sessionid",
    "sessid",
    "socialsecurity",
    "attachment",
];
/// Short roots that must end a word, so `author`, `className` and
/// `max_tokens` stay.
const WORD_ENDS: &[&str] = &[
    "auth",
    "token",
    "session",
    "pwd",
    "ssn",
    "cvv",
    "cvc",
    "signature",
];
/// Roots that must be a whole word, so `spinner` and `shipping` stay.
const WORDS: &[&str] = &["pin", "otp"];

/// Form-decoding passes over an assignment key, so a key encoded twice
/// (`p%2561ssword`) decodes as well as one encoded once.
const DECODE_PASSES: usize = 3;
/// The last [`WINDOW`] bytes of `key`, folded to ASCII by [`fold`]. With
/// `decode`, `+` and `%XX` decode as a form decodes them, up to
/// [`DECODE_PASSES`] times.
fn ascii<'b>(key: &'b str, decode: bool, buf: &'b mut [u8; WINDOW]) -> &'b str {
    let bytes = key.as_bytes();
    let tail = &bytes[bytes.len().saturating_sub(WINDOW)..];
    if !decode && tail.is_ascii() {
        return std::str::from_utf8(tail).unwrap_or_default();
    }
    let mut raw = [0; WINDOW];
    raw[..tail.len()].copy_from_slice(tail);
    let mut len = tail.len();
    if decode {
        for _ in 0..DECODE_PASSES {
            let decoded = form_decode(&mut raw[..len]);
            if decoded == len {
                break;
            }
            len = decoded;
        }
    }
    fold(&raw[..len], buf)
}

/// Decodes `+` and `%XX` in place and returns the decoded length. A pass
/// that keeps the length decoded no `%XX`, so a further pass changes
/// nothing.
fn form_decode(bytes: &mut [u8]) -> usize {
    let hex = |b: Option<&u8>| {
        b.and_then(|b| char::from(*b).to_digit(16))
            .and_then(|d| u8::try_from(d).ok())
    };
    let (mut i, mut len) = (0, 0);
    while i < bytes.len() {
        let mut byte = bytes[i];
        if byte == b'+' {
            byte = b' ';
        } else if byte == b'%'
            && let (Some(high), Some(low)) = (hex(bytes.get(i + 1)), hex(bytes.get(i + 2)))
        {
            byte = high * 16 + low;
            i += 2;
        }
        bytes[len] = byte;
        len += 1;
        i += 1;
    }
    len
}

/// `bytes` with look-alike letters folded to ASCII. Fullwidth and styled
/// letters decompose (NFKD), accents and invisible format characters drop,
/// and anything else becomes `-`, which separates words as it did. The
/// fold runs from the end and stops when the buffer is full, as a long key
/// keeps its end, so a character that decomposes into many does not
/// multiply the work.
fn fold<'b>(bytes: &[u8], buf: &'b mut [u8; WINDOW]) -> &'b str {
    if bytes.is_ascii() {
        buf[..bytes.len()].copy_from_slice(bytes);
        return std::str::from_utf8(&buf[..bytes.len()]).unwrap_or_default();
    }
    // Each character or invalid sequence takes at least one byte.
    let mut chars = ['\0'; WINDOW];
    let mut count = 0;
    for chunk in bytes.utf8_chunks() {
        for c in chunk.valid().chars() {
            chars[count] = c;
            count += 1;
        }
        if !chunk.invalid().is_empty() {
            chars[count] = char::REPLACEMENT_CHARACTER;
            count += 1;
        }
    }
    let mut start = WINDOW;
    for &c in chars[..count].iter().rev() {
        // The longest decomposition, U+FDFA, has 18 characters.
        let (mut part, mut len) = ([0; 32], 0);
        fold_char(c, &mut |byte| {
            if len < part.len() {
                part[len] = byte;
                len += 1;
            }
        });
        let take = len.min(start);
        buf[start - take..start].copy_from_slice(&part[len - take..len]);
        start -= take;
        if start == 0 {
            break;
        }
    }
    buf.copy_within(start.., 0);
    std::str::from_utf8(&buf[..WINDOW - start]).unwrap_or_default()
}

fn fold_char(c: char, push: &mut impl FnMut(u8)) {
    if let Ok(byte) = u8::try_from(c)
        && byte.is_ascii()
    {
        return push(byte);
    }
    unicode_normalization::char::decompose_compatible(c, |part| {
        if let Ok(byte) = u8::try_from(part)
            && byte.is_ascii()
        {
            push(byte);
        } else if let Some(latin) = lookalike(part) {
            push(latin);
        } else if !invisible(part) {
            push(b'-');
        }
    });
}

/// The Latin letter a Cyrillic, Greek or other letter looks like. NFKD
/// leaves these alone, as they are letters of their own script.
fn lookalike(c: char) -> Option<u8> {
    Some(match c {
        'а' | 'α' => b'a',
        'А' | 'Α' => b'A',
        'в' | 'β' => b'b',
        'В' | 'Β' => b'B',
        'с' => b'c',
        'С' => b'C',
        'ԁ' => b'd',
        'е' | 'ѐ' | 'ё' | 'ε' => b'e',
        'Е' | 'Ε' => b'E',
        'ɡ' => b'g',
        'н' | 'һ' => b'h',
        'Н' | 'Һ' | 'Η' => b'H',
        'і' | 'ї' | 'ı' | 'ι' => b'i',
        'І' | 'Ї' | 'Ӏ' | 'Ι' => b'I',
        'ј' => b'j',
        'Ј' => b'J',
        'к' | 'κ' => b'k',
        'К' | 'Κ' => b'K',
        'ӏ' => b'l',
        'м' => b'm',
        'М' | 'Μ' => b'M',
        'η' => b'n',
        'Ν' => b'N',
        'о' | 'ο' | 'օ' => b'o',
        'О' | 'Ο' => b'O',
        'р' | 'ρ' => b'p',
        'Р' | 'Ρ' => b'P',
        'ԛ' => b'q',
        'Ԛ' => b'Q',
        'ѕ' => b's',
        'Ѕ' => b'S',
        'т' | 'τ' => b't',
        'Т' | 'Τ' => b'T',
        'υ' | 'ս' => b'u',
        'ν' => b'v',
        'ԝ' | 'ω' => b'w',
        'Ԝ' => b'W',
        'х' | 'χ' => b'x',
        'Х' | 'Χ' => b'X',
        'у' => b'y',
        'У' | 'Υ' => b'Y',
        'Ζ' => b'Z',
        _ => return None,
    })
}

/// Accents and format characters, which do not show between letters.
fn invisible(c: char) -> bool {
    unicode_normalization::char::is_combining_mark(c)
        || matches!(
            c,
            '\u{00AD}'
                | '\u{180E}'
                | '\u{200B}'..='\u{200F}'
                | '\u{202A}'..='\u{202E}'
                | '\u{2060}'..='\u{2064}'
                | '\u{FEFF}'
        )
}

/// The lowercase ASCII letters and digits of `ascii`.
fn normalize<'b>(ascii: &str, buf: &'b mut [u8; WINDOW]) -> &'b str {
    let mut len = 0;
    for byte in ascii.bytes().filter(u8::is_ascii_alphanumeric) {
        buf[len] = byte.to_ascii_lowercase();
        len += 1;
    }
    std::str::from_utf8(&buf[..len]).unwrap_or_default()
}

/// The form an extra key is stored and compared in.
pub(super) fn normalized(key: &str) -> String {
    let (mut folded, mut out) = ([0; WINDOW], [0; WINDOW]);
    normalize(ascii(key, false, &mut folded), &mut out).to_owned()
}

impl Policy {
    /// The marker for the value under `key`, if the key names one to remove.
    /// `identity` says the key is in a `user` object or feedback.
    pub(super) fn class(&self, key: &str, identity: bool) -> Option<&'static str> {
        let (mut folded, mut out) = ([0; WINDOW], [0; WINDOW]);
        let ascii = ascii(key, false, &mut folded);
        let normalized = normalize(ascii, &mut out);
        if self.sensitive_words(ascii, normalized) {
            return Some(super::SENSITIVE);
        }
        if self.personal
            && identity
            && matches!(
                normalized,
                "id" | "name" | "username" | "fullname" | "userid"
            )
        {
            return Some("[Filtered: personal]");
        }
        if self.email && matches!(normalized, "email" | "contactemail") {
            return Some(super::EMAIL);
        }
        None
    }

    /// Whether the key of an assignment in text, which may be form-encoded,
    /// names a sensitive value.
    pub(super) fn sensitive_assignment(&self, key: &str) -> bool {
        let (mut folded, mut out) = ([0; WINDOW], [0; WINDOW]);
        let ascii = ascii(key, key.contains(['%', '+']), &mut folded);
        self.sensitive_words(ascii, normalize(ascii, &mut out))
    }

    /// Whether the key is an operator's extra key, which applies whether or
    /// not the `keys` class is on, or one of the built-in roots.
    fn sensitive_words(&self, ascii: &str, normalized: &str) -> bool {
        self.extra_keys.contains(normalized) || (self.keys && builtin(ascii, normalized))
    }
}

fn builtin(ascii: &str, normalized: &str) -> bool {
    ROOTS.iter().any(|root| normalized.contains(root))
        || key_words(ascii).any(|word| {
            WORDS.iter().any(|root| word.eq_ignore_ascii_case(root))
                || WORD_ENDS.iter().any(|root| {
                    word.get(word.len().saturating_sub(root.len())..)
                        .is_some_and(|tail| tail.eq_ignore_ascii_case(root))
                })
        })
}

/// The words of an ASCII key, split at separators and camelCase humps.
fn key_words(key: &str) -> impl Iterator<Item = &str> {
    key.split(|c: char| !c.is_ascii_alphanumeric())
        .flat_map(|part| {
            let mut start = 0;
            (1..=part.len()).filter_map(move |end| {
                let bytes = part.as_bytes();
                let boundary = end == part.len()
                    || (bytes[end].is_ascii_uppercase()
                        && (bytes[end - 1].is_ascii_lowercase()
                            || (end + 1 < part.len() && bytes[end + 1].is_ascii_lowercase())));
                boundary.then(|| {
                    let word = &part[start..end];
                    start = end;
                    word
                })
            })
        })
}

/// Stored extra keys, read as they are so that a policy written by another
/// version still applies.
pub(super) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<HashSet<String>, D::Error> {
    Ok(Vec::<String>::deserialize(d)?
        .iter()
        .map(|key| normalized(key))
        .filter(|key| !key.is_empty())
        .collect())
}

/// Extra keys in sorted order, so the same policy always reads the same.
pub(super) fn serialize<S: Serializer>(keys: &HashSet<String>, s: S) -> Result<S::Ok, S::Error> {
    let mut sorted: Vec<&String> = keys.iter().collect();
    sorted.sort_unstable();
    sorted.serialize(s)
}
