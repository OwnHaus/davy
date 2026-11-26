//! The gate ingest asks before it parses a request: today's counts of
//! each project against its quota, in memory, behind one lock.

use super::{Settings, capped};
use crate::{
    time,
    usage::{Count, Signal},
};
use jiff::civil::Date;
use std::{
    collections::HashMap,
    sync::{Mutex, PoisonError},
};

/// Projects' settings and today's counts.
#[derive(Default)]
pub struct Gate(Mutex<State>);

#[derive(Default)]
struct State {
    /// The UTC day the counts are of, and the ns at its end.
    day: Option<(Date, i64)>,
    settings: HashMap<i64, Settings>,
    today: HashMap<i64, Today>,
    refused: HashMap<(Date, i64, &'static str, &'static str), u64>,
    requests: HashMap<(Date, i64, &'static str), u64>,
}

/// One project's day.
#[derive(Default)]
struct Today {
    /// Error events and log bytes accepted today, in the order of
    /// [`capped`].
    used: [u64; 2],
}

/// What a project may send now, read once per request.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Limits {
    /// Errors and logs past today's quota, in the order of [`capped`].
    pub quota: [bool; 2],
    /// Whole seconds until midnight UTC, when the quotas start again.
    pub midnight: u64,
}

impl Limits {
    /// Whether an item of `signal` is refused now.
    pub fn blocks(&self, signal: Signal) -> bool {
        capped(signal).is_some_and(|i| self.quota[i])
    }
    /// Whether anything is refused now.
    pub fn any(&self) -> bool {
        self.quota.contains(&true)
    }
}

/// Items of one category refused on one day, counted in `client_reports`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub day: Date,
    pub project: i64,
    pub reason: &'static str,
    pub category: &'static str,
    pub quantity: i64,
}

/// Requests refused on one day for one reason before their body was
/// read, so with no count of the items they held, counted in
/// `refused_requests`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Requests {
    pub day: Date,
    pub project: i64,
    pub reason: &'static str,
    pub quantity: i64,
}

/// What the gate hands to the writer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Flush {
    pub refused: Vec<Refusal>,
    pub requests: Vec<Requests>,
}

impl Flush {
    pub fn is_empty(&self) -> bool {
        self.refused.is_empty() && self.requests.is_empty()
    }
}

impl State {
    /// Starts a new day of counts when `now_ns` is past the day held.
    fn roll(&mut self, now_ns: i64) -> (Date, i64) {
        match self.day {
            Some((day, end)) if now_ns < end => (day, end),
            _ => {
                let day = (time::day_of(now_ns), time::next_midnight(now_ns));
                self.day = Some(day);
                self.today.clear();
                day
            }
        }
    }
}

impl Gate {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Replaces every project's settings: a project not in `all` has the
    /// defaults.
    pub fn load(&self, all: HashMap<i64, Settings>) {
        self.state().settings = all;
    }

    /// Adds the counts of `day` read back from `usage_daily` at start.
    /// Counts of another day are left out.
    pub fn restore(&self, day: Date, counted: &HashMap<(i64, Signal), Count>, now_ns: i64) {
        let mut s = self.state();
        if s.roll(now_ns).0 != day {
            return;
        }
        for (&(project, signal), &count) in counted {
            if let Some(i) = capped(signal) {
                let today = s.today.entry(project).or_default();
                today.used[i] = today.used[i].saturating_add(amount(i, count));
            }
        }
    }

    /// What `project` may send at `now_ns`.
    pub fn limits(&self, project: i64, now_ns: i64) -> Limits {
        let mut s = self.state();
        let (_, end) = s.roll(now_ns);
        let mut limits = Limits {
            midnight: time::retry_secs(now_ns, end),
            ..Limits::default()
        };
        let Some(settings) = s.settings.get(&project).copied() else {
            return limits;
        };
        let used = s.today.get(&project).map_or([0; 2], |t| t.used);
        for (i, blocked) in limits.quota.iter_mut().enumerate() {
            *blocked = settings.cap(i).is_some_and(|cap| used[i] >= cap);
        }
        limits
    }

    /// Counts what ingest accepted for `project` at `now_ns`. The usage
    /// counter calls it with each count it adds.
    pub fn used(&self, project: i64, signal: Signal, count: Count, now_ns: i64) {
        let Some(i) = capped(signal) else {
            return;
        };
        let mut s = self.state();
        s.roll(now_ns);
        let today = s.today.entry(project).or_default();
        today.used[i] = today.used[i].saturating_add(amount(i, count));
    }

    /// Counts `quantity` items of `category` that `project` sent and the
    /// gate refused for `reason` at `now_ns`.
    pub fn refuse(
        &self,
        project: i64,
        reason: &'static str,
        category: &'static str,
        quantity: u64,
        now_ns: i64,
    ) {
        if quantity == 0 {
            return;
        }
        let mut s = self.state();
        let (day, _) = s.roll(now_ns);
        let n = s
            .refused
            .entry((day, project, reason, category))
            .or_default();
        *n = n.saturating_add(quantity);
    }

    /// Counts one request of `project` refused for `reason` at `now_ns`
    /// before its body was read.
    pub fn refuse_request(&self, project: i64, reason: &'static str, now_ns: i64) {
        let mut s = self.state();
        let (day, _) = s.roll(now_ns);
        let n = s.requests.entry((day, project, reason)).or_default();
        *n = n.saturating_add(1);
    }

    /// Takes what is to be written out, sorted so tests can compare.
    pub fn take(&self) -> Flush {
        let mut s = self.state();
        let mut refused: Vec<Refusal> = s
            .refused
            .drain()
            .map(|((day, project, reason, category), n)| Refusal {
                day,
                project,
                reason,
                category,
                quantity: i64::try_from(n).unwrap_or(i64::MAX),
            })
            .collect();
        refused.sort_by_key(|r| (r.day, r.project, r.reason, r.category));
        let mut requests: Vec<Requests> = s
            .requests
            .drain()
            .map(|((day, project, reason), n)| Requests {
                day,
                project,
                reason,
                quantity: i64::try_from(n).unwrap_or(i64::MAX),
            })
            .collect();
        requests.sort_by_key(|r| (r.day, r.project, r.reason));
        Flush { refused, requests }
    }

    /// Puts back what the writer did not commit.
    pub fn put_back(&self, flush: Flush) {
        let mut s = self.state();
        for r in flush.refused {
            let n = s
                .refused
                .entry((r.day, r.project, r.reason, r.category))
                .or_default();
            *n = n.saturating_add(u64::try_from(r.quantity).unwrap_or(0));
        }
        for r in flush.requests {
            let n = s.requests.entry((r.day, r.project, r.reason)).or_default();
            *n = n.saturating_add(u64::try_from(r.quantity).unwrap_or(0));
        }
    }
}

/// What a count adds to the quota in place `i`: events for errors, bytes
/// for logs.
fn amount(i: usize, count: Count) -> u64 {
    if i == 0 { count.records } else { count.bytes }
}
