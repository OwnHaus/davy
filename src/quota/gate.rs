//! The gate ingest asks before it parses a request: today's counts of
//! each project against its quota, in memory, behind one lock.

use super::{CAPPED, Settings, capped, status::percent};
use crate::{
    time,
    usage::{Count, Signal},
};
use jiff::civil::Date;
use std::{
    collections::{HashMap, HashSet},
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
    /// The alerts of today handed to the writer or read back from
    /// `quota_alerts`, by project, place in [`capped`], kind and quota,
    /// so a raised quota alerts again when it is reached.
    fired: HashSet<(i64, usize, Crossed, u64)>,
    alerts: Vec<Alert>,
}

/// Which line of a quota a project crossed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Crossed {
    /// The warn percent.
    Warn,
    /// 100%.
    Reached,
}

impl Crossed {
    /// The `kind` column of `quota_alerts`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Warn => "warn",
            Self::Reached => "reached",
        }
    }
    /// The kind a `quota_alerts` row names.
    pub fn of(name: &str) -> Option<Self> {
        [Self::Warn, Self::Reached]
            .into_iter()
            .find(|c| c.name() == name)
    }
}

/// A quota a project crossed today, which the writer alerts on once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Alert {
    pub day: Date,
    pub project: i64,
    pub signal: Signal,
    pub crossed: Crossed,
    /// Error events or log bytes used, and the quota, when it crossed.
    pub used: u64,
    pub quota: u64,
    pub warn: u8,
    /// When the gate saw it, and the midnight UTC that ends `day`, in ns.
    pub at: i64,
    pub resets_at: i64,
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
    pub alerts: Vec<Alert>,
}

impl Flush {
    pub fn is_empty(&self) -> bool {
        self.refused.is_empty() && self.requests.is_empty() && self.alerts.is_empty()
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
                self.fired.clear();
                day
            }
        }
    }

    /// Queues an alert when `project`'s use of place `i` crossed a line
    /// of its quota not yet alerted on today. A jump past both lines
    /// queues `Reached` alone, and `Reached` drops a `Warn` still queued.
    fn check(&mut self, project: i64, i: usize, now_ns: i64) {
        let Some(settings) = self.settings.get(&project).copied() else {
            return;
        };
        let Some(quota) = settings.cap(i) else {
            return;
        };
        let used = self.today.get(&project).map_or(0, |t| t.used[i]);
        let crossed = if used >= quota {
            Crossed::Reached
        } else if percent(used, quota) >= u64::from(settings.warn) {
            Crossed::Warn
        } else {
            return;
        };
        if !self.fired.insert((project, i, crossed, quota)) {
            return;
        }
        let (day, resets_at) = self.roll(now_ns);
        if crossed == Crossed::Reached {
            self.fired.insert((project, i, Crossed::Warn, quota));
            self.alerts
                .retain(|a| !(a.day == day && a.project == project && a.signal == CAPPED[i]));
        }
        self.alerts.push(Alert {
            day,
            project,
            signal: CAPPED[i],
            crossed,
            used,
            quota,
            warn: settings.warn,
            at: now_ns,
            resets_at,
        });
    }

    /// [`Self::check`] for every project with a quota.
    fn check_all(&mut self, now_ns: i64) {
        let projects: Vec<i64> = self.settings.keys().copied().collect();
        for project in projects {
            for i in 0..CAPPED.len() {
                self.check(project, i, now_ns);
            }
        }
    }
}

impl Gate {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Replaces every project's settings: a project not in `all` has the
    /// defaults. A lower quota can put a project past a line at once.
    pub fn load(&self, all: HashMap<i64, Settings>, now_ns: i64) {
        let mut s = self.state();
        s.roll(now_ns);
        s.settings = all;
        s.check_all(now_ns);
    }

    /// Marks the alerts of `day` that `quota_alerts` holds as sent, each
    /// with its project, signal, kind and quota.
    pub fn sent(&self, day: Date, rows: &[(i64, Signal, Crossed, u64)], now_ns: i64) {
        let mut s = self.state();
        if s.roll(now_ns).0 != day {
            return;
        }
        for &(project, signal, crossed, quota) in rows {
            if let Some(i) = capped(signal) {
                s.fired.insert((project, i, crossed, quota));
            }
        }
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
        s.check_all(now_ns);
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
        s.check(project, i, now_ns);
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
        let alerts = std::mem::take(&mut s.alerts);
        Flush {
            refused,
            requests,
            alerts,
        }
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
        let mut alerts = flush.alerts;
        alerts.append(&mut s.alerts);
        s.alerts = alerts;
    }
}

/// What a count adds to the quota in place `i`: events for errors, bytes
/// for logs.
fn amount(i: usize, count: Count) -> u64 {
    if i == 0 { count.records } else { count.bytes }
}
