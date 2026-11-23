//! How long a password guess waits after the last one for the same target.
//! Every attempt counts as a failure until it succeeds, so two attempts at
//! once cannot both pass the check. The wait doubles from 1 second to
//! [`MAX_WAIT`], for e-mails that have no user too, so the answer does not
//! tell which e-mails exist.

use super::Email;
use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};

pub const MAX_WAIT: Duration = Duration::from_mins(15);
/// E-mails without a user that are kept. When full, the oldest goes.
///
/// Pushing one out tells a guesser nothing about a user's password, since
/// users count in a table of their own that nothing pushes out. It only
/// lets a guesser who waited out the count of an e-mail see that it has no
/// user. That takes this many attempts within the wait, about 12 minutes
/// at two hashes of 14 ms each. A key of 16 bytes per entry keeps the table
/// under about 10 MB.
pub const CAPACITY: usize = 100_000;

/// Whose password a guess tries.
#[derive(Clone, Copy)]
pub enum Target<'a> {
    /// A user, counted until a success and never pushed out. The users of
    /// one server bound this table.
    User(i64),
    /// An e-mail with no user, counted only so that its answers look like
    /// a user's.
    Unknown(&'a Email),
}

pub struct Backoff {
    users: HashMap<i64, Entry>,
    unknown: HashMap<u128, Entry>,
    /// The keys of `unknown`, oldest first.
    order: VecDeque<u128>,
    capacity: usize,
}

struct Entry {
    attempts: u32,
    until: Instant,
}

impl Backoff {
    pub fn new(capacity: usize) -> Self {
        Self {
            users: HashMap::new(),
            unknown: HashMap::new(),
            order: VecDeque::new(),
            capacity,
        }
    }

    /// Starts an attempt on `target` at `now`, or returns how long it must
    /// wait first.
    pub fn attempt(&mut self, target: Target<'_>, now: Instant) -> Result<(), Duration> {
        let fresh = Entry {
            attempts: 0,
            until: now,
        };
        let entry = match target {
            Target::User(id) => self.users.entry(id).or_insert(fresh),
            Target::Unknown(email) => {
                let key = key(email);
                if !self.unknown.contains_key(&key) {
                    if self.order.len() >= self.capacity
                        && let Some(oldest) = self.order.pop_front()
                    {
                        self.unknown.remove(&oldest);
                    }
                    self.order.push_back(key);
                }
                self.unknown.entry(key).or_insert(fresh)
            }
        };
        if entry.until > now {
            return Err(entry.until - now);
        }
        entry.attempts = entry.attempts.saturating_add(1);
        entry.until = now + wait(entry.attempts);
        Ok(())
    }

    /// How long an attempt on `target` at `now` must wait, without counting
    /// one, so a guess in its wait is refused before it takes a place in the
    /// hash queue.
    pub fn waiting(&self, target: Target<'_>, now: Instant) -> Option<Duration> {
        let entry = match target {
            Target::User(id) => self.users.get(&id),
            Target::Unknown(email) => self.unknown.get(&key(email)),
        }?;
        (entry.until > now).then(|| entry.until - now)
    }

    /// Forgets the attempts on `user` after a right password.
    pub fn succeeded(&mut self, user: i64) {
        self.users.remove(&user);
    }
}

/// 16 bytes of the BLAKE3 hash of `email`, so a full table of long e-mails
/// stays small and a guesser cannot pick e-mails that share a key.
fn key(email: &Email) -> u128 {
    let hash = blake3::hash(email.as_str().as_bytes());
    let mut half = [0; 16];
    half.copy_from_slice(&hash.as_bytes()[..16]);
    u128::from_le_bytes(half)
}

fn wait(attempts: u32) -> Duration {
    let secs = 1_u64
        .checked_shl(attempts.saturating_sub(1))
        .unwrap_or(u64::MAX);
    Duration::from_secs(secs).min(MAX_WAIT)
}
