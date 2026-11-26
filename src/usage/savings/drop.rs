//! What drop rules could save (`docs/design.md`, "Drop rules"): a share to
//! keep of a pattern no rule names yet, and what each shadow rule would
//! drop. The view only reads: a person makes a rule live, and the view
//! names no command for a rule that would hide warnings, errors or Davy's
//! own lines.

use super::{Pattern, Savings, human};
use crate::{
    drops::{self, Match, Set, Spec, Stored},
    install::sh,
    logs::{LogLevel, LogRecord, LogSource, patterns},
    time,
    usage::Count,
};
use anyhow::Result;
use jiff::civil::Date;
use rusqlite::Connection;
use serde_json::{Value, json};

/// The share of a pattern to keep that nobody searched for.
pub const KEEP_UNREAD: u8 = 1;
/// The share of a pattern to keep that a search asked for.
pub const KEEP_READ: u8 = 10;

/// A share of a pattern's lines to keep, and the raw bytes a day dropping
/// the rest saves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Keep {
    pub percent: u8,
    pub saves_per_day: u64,
}

/// What to keep of pattern `p` of one project, given the highest level of
/// its lines and the project's rules, or `None` when its lines reach warn,
/// as dropping those hides trouble, or a rule names it already.
pub fn keep(p: &Pattern, highest: Option<u8>, rules: &[Stored]) -> Option<Keep> {
    let id = patterns::parse(&p.id).ok()?;
    if highest.is_none_or(|l| l >= LogLevel::Warn as u8)
        || rules.iter().any(|r| r.spec.matches == Match::Pattern(id))
    {
        return None;
    }
    let percent = if p.last_queried.is_some() {
        KEEP_READ
    } else {
        KEEP_UNREAD
    };
    let saves = u128::from(p.per_day) * u128::from(100 - percent) / 100;
    Some(Keep {
        percent,
        saves_per_day: u64::try_from(saves).unwrap_or(u64::MAX),
    })
}

/// The patterns of `found`, each with its highest level, with what to
/// keep of each in the view of `project` and the live rule that decides
/// its lines, where the view suggests a rule. `None` is the view of every
/// project, or of Davy's own.
pub fn with_keep(
    found: Vec<(Pattern, Option<u8>)>,
    project: Option<i64>,
    rules: &[Stored],
) -> Vec<Pattern> {
    let set = Set::compile(rules);
    found
        .into_iter()
        .map(|(mut p, highest)| {
            if let Some(project) = project {
                p.keep = keep(&p, highest, rules);
                if p.keep.is_some() {
                    p.decided_by = decided_by(&set, project, &p, highest);
                    p.keep = p.keep.filter(|_| p.decided_by.is_none());
                }
            }
            p
        })
        .collect()
}

/// The live rule of `set` that decides the lines of pattern `p` of
/// project `project`, tried on the pattern's example at the highest level
/// of its lines and with its most frequent service.
pub fn decided_by(set: &Set, project: i64, p: &Pattern, highest: Option<u8>) -> Option<i64> {
    let level = LogLevel::from_code(u64::from(highest?));
    set.first_live(&LogRecord {
        project_id: project,
        timestamp: 0,
        observed_at: 0,
        source: LogSource::Otlp,
        level,
        level_unknown: false,
        message: p.example.clone(),
        service: p.service.clone(),
        trace_id: None,
        span_id: None,
        attributes: serde_json::Map::new(),
    })
}

/// A shadow rule and what it would have dropped in the window.
#[derive(Debug, PartialEq)]
pub struct Shadow {
    pub rule: Stored,
    pub project: String,
    /// The rule is on Davy's own project.
    pub own: bool,
    pub would_drop: Count,
    /// The hours it has counted in the window: since the later of the
    /// start of its first counted day and the time it was made.
    pub hours: f64,
    /// Bytes a day, once it has counted a full day.
    pub per_day: Option<u64>,
}

/// The milliseconds of an hour.
const HOUR_MS: i64 = time::HOUR_NS / 1_000_000;

/// The shadow rules of `rules`, most bytes first, at most `top`, with
/// what each would have dropped from `since` to `now_ms`. `own` is Davy's
/// own project.
pub fn shadows(
    c: &Connection,
    rules: Vec<Stored>,
    (slugs, own): (&dyn Fn(i64) -> String, Option<i64>),
    since: Date,
    now_ms: i64,
    top: usize,
) -> Result<Vec<Shadow>> {
    let counted = drops::counts::by_rule(c, since, None)?;
    let window = time::midnight(since) / 1_000_000;
    let mut out: Vec<Shadow> = rules
        .into_iter()
        .filter(|r| !r.live)
        .map(|rule| {
            let n = counted.get(&rule.id).copied().unwrap_or_default();
            let first = n.first.map_or(window, |d| time::midnight(d) / 1_000_000);
            let from = first.max(window).max(rule.created_at);
            #[expect(
                clippy::cast_precision_loss,
                reason = "milliseconds of a window fit an f64"
            )]
            let hours = (now_ms - from).max(0) as f64 / HOUR_MS as f64;
            let per_day = (hours >= 24.0)
                .then(|| super::super::report::bytes_per_day(n.would_drop.bytes, hours / 24.0));
            Shadow {
                project: slugs(rule.project),
                own: own == Some(rule.project),
                would_drop: n.would_drop,
                hours,
                per_day,
                rule,
            }
        })
        .collect();
    out.sort_by(|a, b| {
        b.would_drop
            .bytes
            .cmp(&a.would_drop.bytes)
            .then(a.rule.id.cmp(&b.rule.id))
    });
    out.truncate(top);
    Ok(out)
}

/// The rule that keeps `keep` of pattern `id`.
fn spec(id: &str, keep: Keep) -> Option<Spec> {
    Some(Spec {
        matches: Match::Pattern(patterns::parse(id).ok()?),
        service: None,
        keep: keep.percent,
        any_level: false,
    })
}

/// What shadow rule `s` would hide that the view names no command for:
/// Davy's own lines, or warnings and errors.
pub fn hides(s: &Shadow) -> Option<String> {
    let spec = &s.rule.spec;
    let what = match &spec.matches {
        _ if !spec.hides_warnings() => None,
        Match::Pattern(_) => Some("every line of the pattern, warnings and errors included"),
        Match::Level(_) => Some("every line at or below the level, warnings included"),
        Match::Message(_) => Some("every line that holds the text, warnings and errors included"),
    };
    let own = s
        .own
        .then_some("lines of Davy's own project, which davy doctor and project_status read");
    match (own, what) {
        (None, None) => None,
        (own, what) => Some(
            [own, what]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join("; "),
        ),
    }
}

impl Savings {
    /// The command that adds the live rule `keep` suggests for `p`,
    /// options first and the slug after `--`.
    pub(super) fn drop_command(&self, p: &Pattern, keep: Keep) -> Option<String> {
        Some(format!(
            "davy drop add {} --data {} -- {}",
            spec(&p.id, keep)?.options(),
            self.data,
            sh(p.project.as_deref()?)
        ))
    }

    /// What a person may do about `p`.
    pub(super) fn drop_suggest(&self, p: &Pattern, keep: Keep) -> Option<String> {
        Some(format!(
            "keep {}%; a person can run: {}",
            keep.percent,
            self.drop_command(p, keep)?
        ))
    }

    /// The command that makes shadow rule `id` live.
    pub(super) fn live_command(&self, id: i64) -> String {
        format!("davy drop live --data {} {id}", self.data)
    }

    /// The fields a pattern entry gains: what to keep, the rule in words
    /// and the command a person runs to add it, what it saves and the
    /// shadow rule an agent adds to count it first, all null when the view
    /// suggests nothing.
    pub(super) fn pattern_drop(&self, p: &Pattern, size: &dyn Fn(u64) -> Value) -> Value {
        if let Some(id) = p.decided_by {
            return json!({
                "suggest": decided(id),
                "rule": null,
                "command": null,
                "saves_per_day": null,
                "shadow": null,
            });
        }
        let Some(keep) = p.keep else {
            return json!({
                "suggest": null,
                "rule": null,
                "command": null,
                "saves_per_day": null,
                "shadow": null,
            });
        };
        json!({
            "suggest": self.drop_suggest(p, keep),
            "rule": spec(&p.id, keep).map(|s| drops::describe(&s)),
            "command": self.drop_command(p, keep),
            "saves_per_day": size(keep.saves_per_day),
            "shadow": format!(
                "drop_rule_shadow project={} pattern={} keep={}",
                Value::from(p.project.as_deref()),
                Value::from(p.id.as_str()),
                Value::from(format!("{}%", keep.percent))
            ),
        })
    }

    /// A shadow entry.
    pub(super) fn shadow_json(&self, s: &Shadow, size: &dyn Fn(u64) -> Value) -> Value {
        let r = &s.rule;
        let (pattern, level, message) = match &r.spec.matches {
            Match::Pattern(id) => (Some(patterns::show(*id)), None, None),
            Match::Level(l) => (None, Some(l.as_str()), None),
            Match::Message(m) => (None, None, Some(m.as_str())),
        };
        json!({
            "kind": "shadow",
            "id": r.id,
            "project": s.project,
            "by": r.by.as_str(),
            "pattern": pattern,
            "level": level,
            "message": message,
            "service": r.spec.service,
            "keep_percent": r.spec.keep,
            "created_at": time::rfc3339_ms(r.created_at),
            "expires_at": r.expires_at.map(time::rfc3339_ms),
            "hours": (s.hours * 10.0).round() / 10.0,
            "would_drop": {"records": s.would_drop.records, "bytes": size(s.would_drop.bytes)},
            "saves_per_day": s.per_day.map(size),
            "hides": hides(s),
            "suggest": hides(s).is_none().then(|| {
                format!("make it live; a person can run: {}", self.live_command(r.id))
            }),
            "rule": drops::describe(&r.spec),
            "command": hides(s).is_none().then(|| self.live_command(r.id)),
        })
    }

    /// The lines of a shadow entry in a terminal.
    pub(super) fn shadow_text(&self, s: &Shadow) -> [String; 2] {
        let counted = match s.per_day {
            Some(per_day) => format!("in {:.1} days, {}/day", s.hours / 24.0, human(per_day)),
            None => format!(
                "in its first {:.1} hours, per day after a full day",
                s.hours
            ),
        };
        let then = match hides(s) {
            Some(hides) => format!("it would hide {hides}"),
            None => format!("a person can run: {}", self.live_command(s.rule.id)),
        };
        [
            format!(
                "rule {} in {} by {}: {}: would drop {} lines ({}) {counted}",
                s.rule.id,
                crate::text::printable(&s.project, char::REPLACEMENT_CHARACTER),
                s.rule.by.as_str(),
                drops::describe(&s.rule.spec),
                s.would_drop.records,
                human(s.would_drop.bytes),
            ),
            crate::text::printable(&then, char::REPLACEMENT_CHARACTER).into_owned(),
        ]
    }
}

/// What the view says of a pattern whose lines live rule `id` decides.
pub(super) fn decided(id: i64) -> String {
    format!("an earlier rule decides these lines: rule {id}")
}
