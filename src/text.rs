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
