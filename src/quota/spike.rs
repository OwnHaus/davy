//! Spike protection (`docs/design.md`, "Spike protection"): a token
//! bucket per project and signal on items a second, kept as a generic
//! cell rate algorithm on one number: the time each bucket is full again.

use std::collections::HashMap;

/// Seconds of its rate a quiet project may send at once, and the longest
/// one request can make it wait.
pub const BURST_SECS: i64 = 10;
const SECOND: i64 = 1_000_000_000;
const BURST: i64 = BURST_SECS * SECOND;

/// Every bucket, by project and place in [`super::capped`].
#[derive(Default)]
pub(super) struct Buckets(HashMap<(i64, usize), i64>);

/// Nanoseconds `items` take at `rate` items a second.
fn cost(items: u64, rate: u64) -> i64 {
    let ns = u128::from(items) * 1_000_000_000 / u128::from(rate.max(1));
    i64::try_from(ns).unwrap_or(i64::MAX)
}

impl Buckets {
    /// Takes `items` that `key` sent at `now_ns` from its bucket, which
    /// goes into debt when they are more than it holds. The debt stops
    /// where the bucket takes items again [`BURST_SECS`] after `now_ns`,
    /// so one request never locks a project out longer than that.
    pub(super) fn charge(&mut self, key: (i64, usize), rate: Option<u64>, items: u64, now_ns: i64) {
        let Some(rate) = rate else {
            self.0.remove(&key);
            return;
        };
        let most = now_ns
            .saturating_add(2 * BURST)
            .saturating_sub(cost(1, rate));
        let full = self.0.entry(key).or_insert(now_ns);
        *full = (*full)
            .max(now_ns)
            .saturating_add(cost(items, rate))
            .min(most.max(now_ns));
    }

    /// When `key`'s bucket cannot take one more item at `now_ns`, the ns
    /// at which it can.
    pub(super) fn refused_until(
        &self,
        key: (i64, usize),
        rate: Option<u64>,
        now_ns: i64,
    ) -> Option<i64> {
        let rate = rate?;
        let full = *self.0.get(&key)?;
        let until = full.saturating_add(cost(1, rate)).saturating_sub(BURST);
        (until > now_ns).then_some(until)
    }

    /// Forgets `key`'s bucket, which is full the next time it sends.
    pub(super) fn forget(&mut self, key: (i64, usize)) {
        self.0.remove(&key);
    }

    /// Forgets buckets that are full again at `now_ns`, so the map holds
    /// only what was sent in the last [`BURST_SECS`].
    pub(super) fn prune(&mut self, now_ns: i64) {
        self.0.retain(|_, full| *full > now_ns);
    }
}
