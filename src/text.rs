//! Text helpers shared by the CLI, alerts and checks.

use std::borrow::Cow;

/// `s` with every control character replaced by `with`, so text from outside
/// cannot move the cursor, retitle a terminal or split a line.
pub fn printable(s: &str, with: char) -> Cow<'_, str> {
    if s.chars().any(char::is_control) {
        Cow::Owned(
            s.chars()
                .map(|c| if c.is_control() { with } else { c })
                .collect(),
        )
    } else {
        Cow::Borrowed(s)
    }
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
