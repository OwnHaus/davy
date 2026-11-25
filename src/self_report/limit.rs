//! How often one kind of line is recorded. A failure that repeats every
//! second, such as a full disk, would otherwise fill Davy's own project
//! with the same line.

use std::collections::HashMap;

/// Lines recorded per component and pattern in one window.
pub const PER_WINDOW: u32 = 10;
/// The window, an hour in nanoseconds.
pub const WINDOW_NS: i64 = 3_600_000_000_000;
/// Components and patterns counted at once. Past this, after the windows
/// that ended are forgotten, every new one shares the window of
/// [`OVERFLOW`].
pub const MAX_KEYS: usize = 1024;
/// The key of the lines of every component and pattern past [`MAX_KEYS`].
pub const OVERFLOW: Key = ("", u64::MAX);

/// A component and the pattern id of a message.
pub type Key = (&'static str, u64);

#[derive(Debug, PartialEq, Eq)]
pub enum Admit {
    /// Record the line, with the lines of its key counted since the last
    /// one recorded.
    Record { suppressed: u64 },
    /// Count the line with the next one recorded.
    Count,
}

#[derive(Default)]
pub struct Limiter {
    windows: HashMap<Key, Window>,
    /// Lines counted in windows that were forgotten before a line of their
    /// key carried the count.
    lost: u64,
}

struct Window {
    start: i64,
    recorded: u32,
    suppressed: u64,
}

impl Limiter {
    /// Whether a line of `key` at `now`, in nanoseconds, is recorded. A
    /// window starts with the first line of its key and lasts
    /// [`WINDOW_NS`]. A clock that went back keeps the window it is in.
    pub fn admit(&mut self, key: Key, now: i64) -> Admit {
        let mut key = key;
        if !self.windows.contains_key(&key) && self.windows.len() >= MAX_KEYS {
            let lost = &mut self.lost;
            self.windows.retain(|_, w| {
                let live = now.saturating_sub(w.start) < WINDOW_NS;
                if !live {
                    *lost = lost.saturating_add(w.suppressed);
                }
                live
            });
            if self.windows.len() >= MAX_KEYS {
                key = OVERFLOW;
            }
        }
        let w = self.windows.entry(key).or_insert(Window {
            start: now,
            recorded: 0,
            suppressed: 0,
        });
        if now.saturating_sub(w.start) >= WINDOW_NS {
            w.start = now;
            w.recorded = 0;
        }
        if w.recorded >= PER_WINDOW {
            w.suppressed = w.suppressed.saturating_add(1);
            return Admit::Count;
        }
        w.recorded += 1;
        Admit::Record {
            suppressed: std::mem::take(&mut w.suppressed),
        }
    }

    /// The lines counted in windows forgotten since the last call, which
    /// no line of their key will carry.
    pub fn take_lost(&mut self) -> u64 {
        std::mem::take(&mut self.lost)
    }
}
