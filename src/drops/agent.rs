//! What an agent may change in the drop rules, over MCP: add a shadow rule
//! and remove one of its own, and, with a token a person unlocked for
//! destructive changes, make a shadow rule live. That change carries the
//! [`Unlocked`] proof, which only an unlocked token makes, so no other job
//! an agent can send writes a live rule, and the writer reads the unlock
//! again before it applies it. No agent stages a rule, or makes
//! one live, on Davy's own project or one that names warnings and errors
//! by level.

use super::{Added, By, Match, Spec};
use crate::access::Unlocked;
use crate::logs::LogLevel;
use crate::store;
use anyhow::Result;
use rusqlite::Connection;

/// A change an agent asked for, applied by the store writer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentChange {
    /// Adds a shadow rule to the project with this slug, or finds the one
    /// that matches the same way.
    Shadow {
        project: String,
        spec: Spec,
        at_ms: i64,
    },
    /// Removes a shadow rule an agent holds in the project with this slug.
    Remove { project: String, rule: i64 },
    /// Makes shadow rule `rule` of the project with this slug live. A
    /// person holds it from then on, as after `davy drop live`: the person
    /// who unlocked the token answers for it, and the ledger names the
    /// token that asked.
    Live {
        project: String,
        rule: i64,
        at_ms: i64,
        unlocked: Unlocked,
    },
}

/// Applies `change`. The outer error is a database failure, the inner one
/// a refusal the agent reads.
pub fn apply(c: &Connection, change: &AgentChange) -> Result<Result<(), String>> {
    let slug = match change {
        AgentChange::Shadow { project, .. }
        | AgentChange::Remove { project, .. }
        | AgentChange::Live { project, .. } => project,
    };
    let Some(project) = store::project_id(c, slug)? else {
        return Ok(Err(format!("unknown project {slug:?}")));
    };
    Ok(match change {
        AgentChange::Shadow { spec, at_ms, .. } => {
            if let Some(refused) = refusal(c, project, slug, spec)? {
                return Ok(Err(refused));
            }
            super::add(c, project, spec, false, By::Agent, *at_ms)?.map(|_: Added| ())
        }
        AgentChange::Remove { rule, .. } => super::remove_agent(c, project, *rule)?,
        AgentChange::Live {
            rule,
            at_ms,
            unlocked,
            ..
        } => {
            if let Some(why) = crate::tokens::refusal(c, *unlocked)? {
                return Ok(Err(why));
            }
            live(c, project, slug, *rule, *at_ms)?
        }
    })
}

/// Makes shadow rule `id` of `project` live, unless an agent may not
/// stage it.
fn live(
    c: &Connection,
    project: i64,
    slug: &str,
    id: i64,
    now_ms: i64,
) -> Result<Result<(), String>> {
    let found = super::get(c, id)?
        .filter(|r| r.project == project && r.expires_at.is_none_or(|at| at > now_ms));
    let Some(rule) = found else {
        return Ok(Err(format!(
            "no rule {id} in project {slug:?}, see drop_rules"
        )));
    };
    if rule.live {
        return Ok(Err(format!("rule {id} is live already")));
    }
    if let Some(refused) = refusal(c, project, slug, &rule.spec)? {
        return Ok(Err(refused));
    }
    Ok(if super::set_live(c, id, true, now_ms)? {
        Ok(())
    } else {
        Err(format!("no rule {id} in project {slug:?}, see drop_rules"))
    })
}

/// Why an agent may not stage `spec` on `project`, if it may not.
fn refusal(c: &Connection, project: i64, slug: &str, spec: &Spec) -> Result<Option<String>> {
    if crate::self_report::own::find(c)?.is_some_and(|own| own.id == project) {
        return Ok(Some(format!(
            "project {slug:?} is Davy's own, whose warnings and errors davy doctor and project_status read: an agent stages no rule on it, a person can with davy drop add"
        )));
    }
    if let Match::Level(level) = spec.matches
        && level >= LogLevel::Warn
    {
        return Ok(Some(format!(
            "a level rule of {} names warnings and errors: an agent stages a level rule below warn only, a person can add this one with davy drop add",
            level.as_str()
        )));
    }
    Ok(None)
}
