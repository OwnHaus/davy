//! The rules as the log writer runs them: compiled once each time they
//! change, then tried on every line of a project that has any. Live rules
//! go first, in order, and the first that matches decides; each shadow
//! rule then counts on its own the lines no live rule matched.

use super::{Key, MAX_RULES, Match, Stored};
use crate::{
    logs::{LogLevel, LogRecord, patterns},
    time,
    usage::Count,
};
use memchr::memmem::Finder;
use rusqlite::Connection;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

/// What a line is matched by, built once per change of the rules.
enum Matcher {
    /// A pattern, and whether warnings and worse match too.
    Pattern(u64, bool),
    Level(LogLevel),
    Contains(Finder<'static>),
    Glob(Glob),
}

/// A message text with `*` in it: the text before the first `*` starts
/// the message, the text after the last ends it, and the pieces between
/// come in order.
struct Glob {
    start: Box<[u8]>,
    end: Box<[u8]>,
    middle: Vec<Finder<'static>>,
}

impl Glob {
    fn new(text: &str) -> Self {
        let pieces: Vec<&str> = text.split('*').collect();
        let (start, end) = (pieces[0], pieces[pieces.len() - 1]);
        let middle = pieces[1..pieces.len() - 1]
            .iter()
            .filter(|p| !p.is_empty())
            .map(|p| Finder::new(p.as_bytes()).into_owned())
            .collect();
        Self {
            start: start.as_bytes().into(),
            end: end.as_bytes().into(),
            middle,
        }
    }

    fn matches(&self, message: &str) -> bool {
        let mut rest = message.as_bytes();
        let Some(after) = rest.strip_prefix(&*self.start) else {
            return false;
        };
        let Some(before) = after.strip_suffix(&*self.end) else {
            return false;
        };
        rest = before;
        for piece in &self.middle {
            match piece.find(rest) {
                Some(at) => rest = &rest[at + piece.needle().len()..],
                None => return false,
            }
        }
        true
    }
}

struct Rule {
    id: i64,
    /// When the rule last changed, so a keep counter starts over.
    changed: i64,
    service: Option<Box<str>>,
    keep: u8,
    matcher: Matcher,
}

impl Rule {
    fn new(s: &Stored) -> Self {
        let matcher = match &s.spec.matches {
            Match::Pattern(id) => Matcher::Pattern(*id, s.spec.any_level),
            Match::Level(l) => Matcher::Level(*l),
            Match::Message(m) if m.contains('*') => Matcher::Glob(Glob::new(m)),
            Match::Message(m) => Matcher::Contains(Finder::new(m.as_bytes()).into_owned()),
        };
        Self {
            id: s.id,
            changed: s.changed_at,
            service: s.spec.service.as_deref().map(Into::into),
            keep: s.spec.keep,
            matcher,
        }
    }

    fn matches(&self, r: &LogRecord, seen: &mut Seen) -> bool {
        if let Some(want) = &self.service
            && r.service.as_deref() != Some(&**want)
        {
            return false;
        }
        match &self.matcher {
            Matcher::Pattern(id, any_level) => {
                (*any_level || r.level < LogLevel::Warn) && seen.pattern(r).id == *id
            }
            // A line whose level Davy did not understand is info to the
            // other rules, but no level rule names it.
            Matcher::Level(l) => !r.level_unknown && r.level <= *l,
            Matcher::Contains(f) => f.find(r.message.as_bytes()).is_some(),
            Matcher::Glob(g) => g.matches(&r.message),
        }
    }
}

#[derive(Default)]
struct ProjectRules {
    live: Vec<Rule>,
    shadow: Vec<Rule>,
}

/// The rules of every project, compiled.
#[derive(Default)]
pub struct Set {
    projects: HashMap<i64, ProjectRules>,
}

impl Set {
    /// Compiles `rules`, at most [`MAX_RULES`] of a project, in the order
    /// given.
    pub fn compile(rules: &[Stored]) -> Self {
        let mut projects: HashMap<i64, ProjectRules> = HashMap::new();
        for s in rules {
            let p = projects.entry(s.project).or_default();
            if p.live.len() + p.shadow.len() >= MAX_RULES {
                continue;
            }
            let side = if s.live { &mut p.live } else { &mut p.shadow };
            side.push(Rule::new(s));
        }
        Self { projects }
    }

    /// The first live rule of `r`'s project that matches `r`, the one that
    /// decides the line.
    pub fn first_live(&self, r: &LogRecord) -> Option<i64> {
        let mut seen = Seen::default();
        self.projects
            .get(&r.project_id)?
            .live
            .iter()
            .find(|rule| rule.matches(r, &mut seen))
            .map(|rule| rule.id)
    }
}

/// What the rules learned of a line, worked out once and only if a rule
/// needs it, so a line with no rule costs nothing more.
#[derive(Default)]
pub struct Seen {
    pattern: Option<patterns::Pattern>,
    bytes: Option<u64>,
}

impl Seen {
    pub fn pattern(&mut self, r: &LogRecord) -> &patterns::Pattern {
        self.pattern.get_or_insert_with(|| patterns::of(&r.message))
    }
    pub fn bytes(&mut self, r: &LogRecord) -> u64 {
        *self
            .bytes
            .get_or_insert_with(|| crate::logs::store::raw_bytes(r))
    }
    /// The pattern, when a rule worked it out, for the pattern counters.
    pub fn take_pattern(&mut self) -> Option<patterns::Pattern> {
        self.pattern.take()
    }
}

#[derive(Default)]
struct Shared {
    generation: AtomicU64,
    set: Mutex<Arc<Set>>,
}

/// The rules in force, shared by whoever reads them from the store and
/// the log writer, which takes a new set at its next batch.
#[derive(Clone, Default)]
pub struct Live(Arc<Shared>);

impl Live {
    /// Puts `set` in force.
    pub fn set(&self, set: Set) {
        *self.0.set.lock().unwrap_or_else(PoisonError::into_inner) = Arc::new(set);
        self.0.generation.fetch_add(1, Ordering::Release);
    }
}

/// The log writer's side: the set it runs, the keep counter of each rule,
/// and what it counted since the last [`Judge::take`].
pub struct Judge {
    live: Live,
    generation: u64,
    set: Arc<Set>,
    /// Per rule and change: adds the rule's keep percent per line it
    /// matched and keeps the line each time it reaches 100.
    keep: HashMap<(i64, i64), u8>,
    counts: HashMap<Key, Count>,
}

impl Judge {
    pub fn new(live: Live) -> Self {
        Self {
            live,
            generation: 0,
            set: Arc::default(),
            keep: HashMap::new(),
            counts: HashMap::new(),
        }
    }

    /// Takes the set put in force since the last call, if any.
    pub fn refresh(&mut self) {
        let now = self.live.0.generation.load(Ordering::Acquire);
        if now == self.generation {
            return;
        }
        self.generation = now;
        self.set = Arc::clone(
            &self
                .live
                .0
                .set
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
        let set = &self.set;
        self.keep.retain(|(id, changed), _| {
            set.projects.values().any(|p| {
                p.live
                    .iter()
                    .chain(&p.shadow)
                    .any(|r| r.id == *id && r.changed == *changed)
            })
        });
    }

    /// Whether to store `r`, counting it for the rule that drops it and
    /// for each shadow rule that would.
    pub fn keep(&mut self, r: &LogRecord, seen: &mut Seen) -> bool {
        let Some(rules) = self.set.projects.get(&r.project_id) else {
            return true;
        };
        let day = time::day_of(r.observed_at);
        for rule in &rules.live {
            if rule.matches(r, seen) {
                let dropped = !step(&mut self.keep, rule);
                if dropped {
                    count(&mut self.counts, day, rule.id, true, r, seen);
                }
                return !dropped;
            }
        }
        for rule in &rules.shadow {
            if rule.matches(r, seen) && !step(&mut self.keep, rule) {
                count(&mut self.counts, day, rule.id, false, r, seen);
            }
        }
        true
    }

    /// What was counted since the last call.
    pub fn take(&mut self) -> HashMap<Key, Count> {
        std::mem::take(&mut self.counts)
    }
}

/// Whether `rule` keeps the line it matched.
fn step(keep: &mut HashMap<(i64, i64), u8>, rule: &Rule) -> bool {
    if rule.keep == 0 {
        return false;
    }
    // Starts one step short of 100, so the first line is kept, then one
    // in every 100 / keep, exactly over any run of lines.
    let acc = keep
        .entry((rule.id, rule.changed))
        .or_insert(100 - rule.keep);
    *acc += rule.keep;
    if *acc >= 100 {
        *acc -= 100;
        true
    } else {
        false
    }
}

fn count(
    counts: &mut HashMap<Key, Count>,
    day: jiff::civil::Date,
    rule: i64,
    live: bool,
    r: &LogRecord,
    seen: &mut Seen,
) {
    let key = Key {
        day,
        rule,
        live,
        project: r.project_id,
    };
    let n = counts.entry(key).or_default();
    n.add(Count {
        records: 1,
        bytes: seen.bytes(r),
    });
}

/// Reads the rules in `c` every `period` and puts them in force when they
/// changed. A read that fails keeps the rules in force and is reported.
pub async fn reload_every(c: Connection, live: Live, period: Duration) {
    let mut ticks = tokio::time::interval(period);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last = None;
    loop {
        ticks.tick().await;
        match super::load(&c, None, time::now_ms()) {
            Ok(rules) if last.as_ref() != Some(&rules) => {
                live.set(Set::compile(&rules));
                last = Some(rules);
            }
            Ok(_) => {}
            Err(e) => crate::self_report::warn("drop rules", format_args!("not read: {e:#}")),
        }
    }
}

/// Reads the rules in `c` once and puts them in force, so a server
/// applies them from its first line.
pub fn reload(c: &Connection, live: &Live) -> anyhow::Result<()> {
    live.set(Set::compile(&super::load(c, None, time::now_ms())?));
    Ok(())
}

#[cfg(test)]
#[path = "judge_tests.rs"]
mod tests;
