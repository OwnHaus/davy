//! Text helpers shared by the CLI, alerts and checks.

use std::borrow::Cow;

/// `s` with every control character and every mark that reorders text
/// replaced by `with`, so text from outside cannot move the cursor, retitle
/// a terminal, split a line, or show its words in another order than they
/// are read.
pub fn printable(s: &str, with: char) -> Cow<'_, str> {
    if s.chars().any(hidden) {
        Cow::Owned(
            s.chars()
                .map(|c| if hidden(c) { with } else { c })
                .collect(),
        )
    } else {
        Cow::Borrowed(s)
    }
}

/// A control character, C0 and C1, or a bidirectional mark, embedding,
/// override or isolate.
fn hidden(c: char) -> bool {
    c.is_control() || reorders(c)
}

/// A bidirectional mark, embedding, override or isolate.
fn reorders(c: char) -> bool {
    matches!(
        c,
        '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
    )
}

/// A character a person cannot see or tell from a space: a control
/// character, a format character (Unicode category Cf: bidi marks, zero
/// width spaces and joiners, U+2060, U+FEFF and the rest), a line or
/// paragraph separator, or a no-break space. Rule text that holds one
/// would look like a rule it is not.
pub fn invisible(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{00A0}'
                | '\u{00AD}'
                | '\u{0600}'..='\u{0605}'
                | '\u{061C}'
                | '\u{06DD}'
                | '\u{070F}'
                | '\u{0890}'..='\u{0891}'
                | '\u{08E2}'
                | '\u{180E}'
                | '\u{200B}'..='\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{206F}'
                | '\u{FEFF}'
                | '\u{FFF9}'..='\u{FFFB}'
                | '\u{110BD}'
                | '\u{110CD}'
                | '\u{13430}'..='\u{1343F}'
                | '\u{1BCA0}'..='\u{1BCA3}'
                | '\u{1D173}'..='\u{1D17A}'
                | '\u{E0001}'
                | '\u{E0020}'..='\u{E007F}'
        )
}

/// `value` as JSON, on one line or with `pretty` indented, safe to print in
/// a terminal and still exact: serde escapes C0 controls, and this writes
/// DEL, C1 controls and every mark [`printable`] replaces as `\uXXXX`
/// escapes too, which JSON reads back as the same characters. Each of them
/// can only stand inside a string, as JSON's own syntax is ASCII.
pub fn json(value: &impl serde::Serialize, pretty: bool) -> serde_json::Result<String> {
    let s = if pretty {
        serde_json::to_string_pretty(value)?
    } else {
        serde_json::to_string(value)?
    };
    if !s.chars().any(escaped) {
        return Ok(s);
    }
    let mut out = String::with_capacity(s.len() + 16);
    for c in s.chars() {
        if escaped(c) {
            out.push_str("\\u");
            let n = u32::from(c);
            for shift in [12, 8, 4, 0] {
                out.extend(char::from_digit((n >> shift) & 0xf, 16));
            }
        } else {
            out.push(c);
        }
    }
    Ok(out)
}

/// What [`json`] escapes that serde leaves raw.
fn escaped(c: char) -> bool {
    ('\u{7f}'..='\u{9f}').contains(&c) || reorders(c)
}

/// `bytes` for a person: `74 B`, `1.2 KB`, `56 MB`, in powers of 1000.
/// One decimal below 100 of a unit, none from there on.
pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["KB", "MB", "GB", "TB", "PB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "a size is shown to three digits"
    )]
    let mut value = bytes as f64 / 1000.0;
    let mut unit = 0;
    while value >= 999.5 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    if value < 99.95 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

/// `bytes` as lowercase hex digits, two per byte.
pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .flat_map(|b| [b >> 4, b & 0xf])
        .filter_map(|nibble| char::from_digit(u32::from(nibble), 16))
        .collect()
}

#[cfg(test)]
#[path = "text_tests.rs"]
mod tests;
