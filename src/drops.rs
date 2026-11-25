//! Drop rules for logs (`docs/design.md`, "Drop rules"): per project, a
//! rule names lines by pattern, level or message text, and the log writer
//! drops them, or all but a share, before they are stored. A shadow rule
//! drops nothing and counts what it would drop. Only a person makes a rule
//! live, with `davy drop`: an agent creates and removes shadow rules of its
//! own over MCP, a few per project, and they expire. No MCP tool, route or
//! ingest path writes a live rule or turns a shadow rule live.

mod cli;
pub mod counts;
mod judge;

pub use cli::{Drop, run};
pub use counts::{Key, filtered_between, write};
pub use judge::{Judge, Live, Seen, Set, reload, reload_every};

use crate::logs::{LogLevel, patterns};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension as _, params};
use std::time::Duration;

/// Rules per project, live and shadow together, so the log writer never
/// tries more than this many on a line.
pub const MAX_RULES: usize = 50;
/// Shadow rules an agent may hold in one project at once.
pub const MAX_AGENT_SHADOW: usize = 10;
/// Days an agent's shadow rule lives.
pub const AGENT_DAYS: i64 = 14;
/// Bytes of a message text or a service name a rule may hold.
pub const MAX_TEXT: usize = 256;
/// How soon a change to the rules reaches the log writer.
pub const RELOAD_EVERY: Duration = Duration::from_secs(5);
const DAY_MS: i64 = 86_400_000;

/// The rules and the lines each one dropped or would drop per day. One
/// migration, so a branch that takes the same number renumbers one entry.
/// Ids are never used twice, so a new rule never inherits the counts of a
/// removed one. Two rules of a project never match the same way. Only a
/// pattern rule has `any_level`.
pub const MIGRATION: &str = "CREATE TABLE drop_rules(id INTEGER PRIMARY KEY AUTOINCREMENT, project_id INTEGER NOT NULL REFERENCES projects(id), position INTEGER NOT NULL, live INTEGER NOT NULL CHECK(live IN (0,1)), by TEXT NOT NULL CHECK(by IN ('person','agent')), pattern INTEGER, level INTEGER, message TEXT, service TEXT, keep_percent INTEGER NOT NULL CHECK(keep_percent BETWEEN 0 AND 99), any_level INTEGER NOT NULL DEFAULT 0 CHECK(any_level IN (0,1)), created_at INTEGER NOT NULL, changed_at INTEGER NOT NULL, expires_at INTEGER, CHECK((pattern IS NOT NULL)+(level IS NOT NULL)+(message IS NOT NULL)=1), CHECK(any_level=0 OR pattern IS NOT NULL));
CREATE UNIQUE INDEX drop_rules_match ON drop_rules(project_id, coalesce(pattern,-1), coalesce(level,-1), coalesce(message,''), coalesce(service,''), keep_percent, any_level);
CREATE TABLE drop_daily(day TEXT NOT NULL, rule_id INTEGER NOT NULL, live INTEGER NOT NULL, project_id INTEGER NOT NULL REFERENCES projects(id), records INTEGER NOT NULL, bytes INTEGER NOT NULL, PRIMARY KEY(day, rule_id, live)) WITHOUT ROWID;";
/// Undoes [`MIGRATION`], for tests that run the migrations again.
pub const UNDO: &str = "DROP TABLE drop_daily; DROP TABLE drop_rules; DELETE FROM sqlite_sequence WHERE name='drop_rules';";

/// What lines a rule names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Match {
    /// Lines of this pattern, see [`patterns::of`].
    Pattern(u64),
    /// Lines of this level and every lower one.
    Level(LogLevel),
    /// Lines whose message holds this text, or with a `*` matches it as a
    /// glob over the whole message.
    Message(String),
}

/// Who holds a rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum By {
    Person,
    Agent,
}

impl By {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Person => "person",
            Self::Agent => "agent",
        }
    }
}

/// A rule without its place: what it names and what it keeps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spec {
    pub matches: Match,
    /// Only lines of this service, exactly.
    pub service: Option<String>,
    /// The percent of the lines it names that it keeps, 0 to drop all.
    pub keep: u8,
    /// A pattern rule matches warnings and worse too. False skips them, so
    /// a pattern that once held only noise cannot hide the error it later
    /// also holds. Only a person sets it, and only on a pattern rule.
    pub any_level: bool,
}

impl Spec {
    /// The rule a person or an agent typed: exactly one of `pattern`,
    /// `level` and `message`.
    pub fn new(
        pattern: Option<&str>,
        level: Option<&str>,
        message: Option<&str>,
        service: Option<&str>,
        keep: Option<&str>,
    ) -> Result<Self, String> {
        let matches = match (pattern, level, message) {
            (Some(p), None, None) => match patterns::parse(p)? {
                patterns::OTHER => {
                    return Err("pattern other is no pattern: it counts many kinds of lines".into());
                }
                id => Match::Pattern(id),
            },
            (None, Some(l), None) => Match::Level(LogLevel::named(l)?),
            (None, None, Some(m)) => Match::Message(text("message", m)?),
            _ => return Err("name exactly one of pattern, level and message".into()),
        };
        let service = service.map(|s| text("service", s)).transpose()?;
        let keep = keep.map_or(Ok(0), parse_keep)?;
        Ok(Self {
            matches,
            service,
            keep,
            any_level: false,
        })
    }

    /// Whether the rule can match a warning or worse: a level rule of warn
    /// or above, a message rule, or a pattern rule at any level.
    pub fn hides_warnings(&self) -> bool {
        match &self.matches {
            Match::Pattern(_) => self.any_level,
            Match::Level(l) => *l >= LogLevel::Warn,
            Match::Message(_) => true,
        }
    }

    /// The options of `davy drop add` that make this rule.
    pub fn options(&self) -> String {
        let what = match &self.matches {
            Match::Pattern(id) => format!("--pattern {}", patterns::show(*id)),
            Match::Level(l) => format!("--level {}", l.as_str()),
            Match::Message(m) => format!("--message {}", crate::install::sh(m)),
        };
        let service = self
            .service
            .as_deref()
            .map(|s| format!("--service {}", crate::install::sh(s)));
        let keep = (self.keep > 0).then(|| format!("--keep {}%", self.keep));
        let any = self.any_level.then(|| "--any-level".to_owned());
        [Some(what), service, keep, any]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// A message text or a service: not empty, at most [`MAX_TEXT`] bytes,
/// and no character a person cannot see, see [`crate::text::invisible`].
fn text(what: &str, s: &str) -> Result<String, String> {
    if s.is_empty() {
        return Err(format!("{what} is empty"));
    }
    if s.chars().all(|c| c == '*') {
        return Err(if what == "service" {
            "service is only *: a service is matched exactly, and * is no wildcard in it; leave --service out for every service".into()
        } else {
            format!("{what} is only *, which matches every line: use --level, or name the text")
        });
    }
    if s.len() > MAX_TEXT {
        return Err(format!("{what} is longer than {MAX_TEXT} bytes"));
    }
    if let Some(c) = s.chars().find(|c| crate::text::invisible(*c)) {
        return Err(format!(
            "{what} holds U+{:04X}, which a person cannot see or tell from a space: type it without",
            u32::from(c)
        ));
    }
    Ok(s.to_owned())
}

/// `N%` or `N`, 1 to 99, digits only: the share of the lines a rule keeps.
pub fn parse_keep(s: &str) -> Result<u8, String> {
    let n = s.trim().strip_suffix('%').unwrap_or(s.trim());
    let digits = !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit());
    match n.parse::<u8>() {
        Ok(n @ 1..=99) if digits => Ok(n),
        _ => Err(format!(
            "keep {s:?} is not a percent from 1 to 99; leave it out to drop all"
        )),
    }
}

/// A rule as stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stored {
    pub id: i64,
    pub project: i64,
    pub position: i64,
    pub live: bool,
    pub by: By,
    pub spec: Spec,
    pub created_at: i64,
    pub changed_at: i64,
    pub expires_at: Option<i64>,
}

const COLUMNS: &str = "id, project_id, position, live, by, pattern, level, message, service, keep_percent, created_at, changed_at, expires_at, any_level";

fn stored(r: &rusqlite::Row<'_>) -> rusqlite::Result<Stored> {
    let matches = match (r.get::<_, Option<i64>>(5)?, r.get::<_, Option<i64>>(6)?) {
        (Some(p), _) => Match::Pattern(p.cast_unsigned()),
        (None, Some(l)) => Match::Level(LogLevel::from_code(l.cast_unsigned())),
        (None, None) => Match::Message(r.get(7)?),
    };
    Ok(Stored {
        id: r.get(0)?,
        project: r.get(1)?,
        position: r.get(2)?,
        live: r.get(3)?,
        by: if r.get::<_, String>(4)? == "agent" {
            By::Agent
        } else {
            By::Person
        },
        spec: Spec {
            matches,
            service: r.get(8)?,
            keep: r.get(9)?,
            any_level: r.get(13)?,
        },
        created_at: r.get(10)?,
        changed_at: r.get(11)?,
        expires_at: r.get(12)?,
    })
}

/// The rules in force at `now_ms` of one project or of all, in the order
/// the log writer tries them.
pub fn load(c: &Connection, project: Option<i64>, now_ms: i64) -> Result<Vec<Stored>> {
    let mut q = c.prepare_cached(&format!(
        "SELECT {COLUMNS} FROM drop_rules
         WHERE (?1 IS NULL OR project_id=?1) AND (expires_at IS NULL OR expires_at>?2)
         ORDER BY project_id, position, id"
    ))?;
    Ok(q.query_map(params![project, now_ms], stored)?
        .collect::<rusqlite::Result<_>>()?)
}

/// Rule `id`, expired or not.
pub fn get(c: &Connection, id: i64) -> Result<Option<Stored>> {
    Ok(c.query_row(
        &format!("SELECT {COLUMNS} FROM drop_rules WHERE id=?1"),
        [id],
        stored,
    )
    .optional()?)
}

/// What [`add`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Added {
    pub id: i64,
    /// False when a rule of the project matched the same way already, and
    /// that one is the rule.
    pub created: bool,
}

/// Adds a rule to `project` after its others, or finds the one that
/// matches the same way. The outer error is a database failure, the inner
/// one a refusal. An agent adds only shadow rules, [`MAX_AGENT_SHADOW`] at
/// once, each for [`AGENT_DAYS`]. Runs inside the caller's transaction.
pub fn add(
    c: &Connection,
    project: i64,
    spec: &Spec,
    live: bool,
    by: By,
    now_ms: i64,
) -> Result<Result<Added, String>> {
    if live && by == By::Agent {
        return Ok(Err(
            "an agent adds shadow rules only: a person makes a rule live".into(),
        ));
    }
    c.execute(
        "DELETE FROM drop_rules WHERE project_id=?1 AND expires_at<=?2",
        params![project, now_ms],
    )?;
    if let Some(id) = same(c, project, spec)? {
        return Ok(Ok(Added { id, created: false }));
    }
    let (all, agents): (i64, i64) = c.query_row(
        "SELECT count(*), coalesce(sum(by='agent' AND live=0), 0) FROM drop_rules WHERE project_id=?1",
        [project],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if usize::try_from(all).unwrap_or(usize::MAX) >= MAX_RULES {
        return Ok(Err(format!(
            "the project has {MAX_RULES} rules, the most it may hold: remove one first"
        )));
    }
    if by == By::Agent && usize::try_from(agents).unwrap_or(usize::MAX) >= MAX_AGENT_SHADOW {
        return Ok(Err(format!(
            "an agent holds {MAX_AGENT_SHADOW} shadow rules in this project, the most it may: remove one first"
        )));
    }
    let (pattern, level, message) = columns(&spec.matches);
    let expires = (by == By::Agent).then(|| now_ms.saturating_add(AGENT_DAYS * DAY_MS));
    c.execute(
        "INSERT INTO drop_rules(project_id, position, live, by, pattern, level, message, service, keep_percent, any_level, created_at, changed_at, expires_at)
         VALUES (?1, (SELECT coalesce(max(position), 0)+1 FROM drop_rules WHERE project_id=?1), ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10, ?11)",
        params![project, live, by.as_str(), pattern, level, message, spec.service, spec.keep, spec.any_level, now_ms, expires],
    )?;
    Ok(Ok(Added {
        id: c.last_insert_rowid(),
        created: true,
    }))
}

/// The pattern, level and message columns of `m`.
fn columns(m: &Match) -> (Option<i64>, Option<i64>, Option<&str>) {
    match m {
        Match::Pattern(id) => (Some(id.cast_signed()), None, None),
        Match::Level(l) => (None, Some(*l as i64), None),
        Match::Message(s) => (None, None, Some(s.as_str())),
    }
}

/// The rule of `project` that matches as `spec` does.
fn same(c: &Connection, project: i64, spec: &Spec) -> Result<Option<i64>> {
    let (pattern, level, message) = columns(&spec.matches);
    Ok(c.query_row(
        "SELECT id FROM drop_rules WHERE project_id=?1 AND coalesce(pattern,-1)=coalesce(?2,-1)
         AND coalesce(level,-1)=coalesce(?3,-1) AND coalesce(message,'')=coalesce(?4,'')
         AND coalesce(service,'')=coalesce(?5,'') AND keep_percent=?6 AND any_level=?7",
        params![
            project,
            pattern,
            level,
            message,
            spec.service,
            spec.keep,
            spec.any_level
        ],
        |r| r.get(0),
    )
    .optional()?)
}

/// Makes rule `id` live after the project's other live rules, or with
/// `live` false a shadow rule. Either way a person holds it from now on and
/// it never expires. False when there is no such rule, or it is an agent's
/// rule that expired at `now_ms`: the log writer no longer runs it, and
/// the next change to the project deletes it.
pub fn set_live(c: &Connection, id: i64, live: bool, now_ms: i64) -> Result<bool> {
    Ok(c.execute(
        "UPDATE drop_rules SET live=?2, by='person', expires_at=NULL, changed_at=?3,
         position=CASE WHEN live=?2 THEN position ELSE
           (SELECT coalesce(max(position), 0)+1 FROM drop_rules d WHERE d.project_id=drop_rules.project_id) END
         WHERE id=?1 AND (expires_at IS NULL OR expires_at>?3)",
        params![id, live, now_ms],
    )? == 1)
}

/// Removes rule `id`. Its counts stay until they are pruned, so the lines
/// it dropped stay in `davy usage`. False when there is no such rule.
pub fn remove(c: &Connection, id: i64) -> Result<bool> {
    Ok(c.execute("DELETE FROM drop_rules WHERE id=?1", [id])? == 1)
}

/// Removes rule `id` of `project` for an agent: only a shadow rule an
/// agent holds, and what it counted: it never dropped a line, so nothing
/// it counted belongs in `davy usage`. The outer error is a database
/// failure, the inner one a refusal.
pub fn remove_agent(c: &Connection, project: i64, id: i64) -> Result<Result<(), String>> {
    let gone = c.execute(
        "DELETE FROM drop_rules WHERE id=?1 AND project_id=?2 AND live=0 AND by='agent'",
        params![id, project],
    )?;
    Ok(if gone == 1 {
        c.execute("DELETE FROM drop_daily WHERE rule_id=?1", [id])?;
        Ok(())
    } else {
        Err(format!(
            "no shadow rule {id} of an agent in this project: a person removes the others with davy drop remove"
        ))
    })
}

#[cfg(test)]
#[path = "drops_tests.rs"]
mod tests;
