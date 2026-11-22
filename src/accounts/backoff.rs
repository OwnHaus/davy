//! How long an e-mail waits between login attempts. Every attempt counts as
//! a failure until it succeeds, so two attempts at once cannot both pass the
//! check. The wait doubles from 1 second to [`MAX_WAIT`], for e-mails that
//! have no user too, so the answer does not tell which e-mails exist.

use super::Email;
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

pub const MAX_WAIT: Duration = Duration::from_mins(15);
/// Entries kept. When full, the one with the fewest attempts goes, and of
/// those the one whose wait ended first. A flood of new e-mails then pushes
/// out other new e-mails, not the count of one under attack.
pub const CAPACITY: usize = 10_000;

pub struct Backoff {
    entries: HashMap<Email, Entry>,
    capacity: usize,
}

struct Entry {
    attempts: u32,
    until: Instant,
}

impl Backoff {
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            capacity,
        }
    }

    /// Starts an attempt for `email` at `now`, or returns how long it must
    /// wait first.
    pub fn attempt(&mut self, email: &Email, now: Instant) -> Result<(), Duration> {
        if let Some(entry) = self.entries.get_mut(email) {
            if entry.until > now {
                return Err(entry.until - now);
            }
            entry.attempts = entry.attempts.saturating_add(1);
            entry.until = now + wait(entry.attempts);
            return Ok(());
        }
        if self.entries.len() >= self.capacity {
            self.evict();
        }
        let entry = Entry {
            attempts: 1,
            until: now + wait(1),
        };
        self.entries.insert(email.clone(), entry);
        Ok(())
    }

    /// Forgets the attempts of `email` after a successful login.
    pub fn succeeded(&mut self, email: &Email) {
        self.entries.remove(email);
    }

    fn evict(&mut self) {
        let least = self
            .entries
            .iter()
            .min_by_key(|(_, entry)| (entry.attempts, entry.until))
            .map(|(email, _)| email.clone());
        if let Some(email) = least {
            self.entries.remove(&email);
        }
    }
}

fn wait(attempts: u32) -> Duration {
    let secs = 1_u64
        .checked_shl(attempts.saturating_sub(1))
        .unwrap_or(u64::MAX);
    Duration::from_secs(secs).min(MAX_WAIT)
}
