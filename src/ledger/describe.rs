//! What a ledger row says a change was about, in the same words whether an
//! agent made it over MCP, a person in the web UI or someone with `davy`.
//! Every text an agent or a person sent is cut short, and a payload, a
//! feedback text or a secret never becomes part of a summary.

use super::cut;
use crate::{
    retention::{self, Setting},
    store::IssueStatus,
    tokens,
};
use serde_json::{Map, Value};

/// The longest release, service or setting a summary quotes.
const MAX_WORD: usize = 40;
/// The longest log message a summary of a rule quotes.
const MAX_MESSAGE: usize = 60;
/// The longest pattern a summary of a rule quotes.
const MAX_PATTERN: usize = 20;
/// Issue IDs a summary of a bulk change lists before `and N more`.
const LISTED: usize = 5;

/// What a change was about.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct What {
    /// The project's slug. For an issue, the writer looks it up.
    pub project: Option<String>,
    pub issue: Option<i64>,
    pub target: Option<String>,
    pub summary: String,
}

/// A call of the MCP `tool` with `args` as sent, so a call the tool
/// refused for its arguments is described all the same.
pub fn tool(tool: &str, args: &Map<String, Value>) -> What {
    let text = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_owned);
    let rule = args.get("rule_id").and_then(Value::as_i64);
    let project = text("project");
    let what = match tool {
        "mark_fixed" => {
            let issue = args.get("issue_id").and_then(Value::as_i64);
            let resolved = IssueStatus::Resolved {
                in_release: text("release"),
                at: 0,
            };
            status(&issue.into_iter().collect::<Vec<_>>(), &resolved)
        }
        "submit_feedback" => said("sent feedback about Davy"),
        "drop_rule_shadow" => shadow(&text),
        "drop_rule_remove" => What {
            target: rule.map(|id| format!("rule {id}")),
            summary: format!("removed shadow {}", rule_words(rule)),
            ..What::default()
        },
        "drop_rule_live" => rule.map_or_else(|| said("made a rule live"), rule_live),
        "set_retention" => serde_json::from_value(Value::Object(args.clone()))
            .map_or_else(|_| said("changed retention"), |c| retention(&c)),
        _ => said(&format!("called {tool}")),
    };
    What { project, ..what }
}

fn said(summary: &str) -> What {
    What {
        summary: summary.to_owned(),
        ..What::default()
    }
}

/// A new status for `issues`, as `mark_fixed`, the web UI and `davy
/// resolve` set it.
pub fn status(issues: &[i64], status: &IssueStatus) -> What {
    let summary = match status {
        IssueStatus::Resolved {
            in_release: Some(r),
            ..
        } => format!("resolved, fixed in release {}", cut(r, MAX_WORD)),
        IssueStatus::Resolved { .. } => "resolved, fixed in the next release".to_owned(),
        IssueStatus::Unresolved => "reopened".to_owned(),
        IssueStatus::Ignored => "ignored".to_owned(),
    };
    match issues {
        [] => What {
            summary,
            ..What::default()
        },
        [one] => What {
            issue: Some(*one),
            target: Some(format!("issue {one}")),
            summary,
            ..What::default()
        },
        many => {
            let mut listed: Vec<String> = many.iter().take(LISTED).map(i64::to_string).collect();
            if many.len() > LISTED {
                listed.push(format!("and {} more", many.len() - LISTED));
            }
            What {
                target: Some(format!("{} issues", many.len())),
                summary: format!("{summary}: issues {}", listed.join(", ")),
                ..What::default()
            }
        }
    }
}

/// A change to how long a project keeps its logs and errors.
pub fn retention(change: &retention::Change) -> What {
    let days = |s: Option<Setting>| match s {
        None => "unchanged".to_owned(),
        Some(Setting::Default(_)) => "the default".to_owned(),
        Some(Setting::Days(n)) => format!("{n} days"),
    };
    What {
        project: Some(change.project.clone()),
        summary: format!(
            "logs {}, errors {}",
            days(change.logs_days),
            days(change.errors_days)
        ),
        ..What::default()
    }
}

/// A shadow rule an agent adds: what it matches, and for which service.
fn shadow(text: &impl Fn(&str) -> Option<String>) -> What {
    let target = text("pattern")
        .map(|p| format!("pattern {}", cut(&p, MAX_PATTERN)))
        .or_else(|| text("level").map(|l| format!("level {} and below", cut(&l, 10))))
        .or_else(|| text("message").map(|m| format!("message {:?}", cut(&m, MAX_MESSAGE))));
    let service = text("service").map(|s| format!(", service {}", cut(&s, MAX_WORD)));
    let keep = text("keep").map(|k| format!(", keeping {}", cut(&k, 8)));
    let summary = format!(
        "added a shadow rule for {}{}{}",
        target.as_deref().unwrap_or("nothing"),
        service.unwrap_or_default(),
        keep.unwrap_or_default()
    );
    What {
        target,
        summary,
        ..What::default()
    }
}

fn rule_words(rule: Option<i64>) -> String {
    rule.map_or_else(|| "rule".to_owned(), |id| format!("rule {id}"))
}

/// A shadow rule made live, by an agent or with `davy drop live`.
pub fn rule_live(id: i64) -> What {
    What {
        target: Some(format!("rule {id}")),
        summary: format!("made rule {id} live"),
        ..What::default()
    }
}

/// A live rule a person added to `project` with `davy drop add`, in the
/// words `davy drop list` uses, with its id once it was added.
pub fn rule_added(project: &str, id: Option<i64>, words: &str) -> What {
    let summary = match id {
        Some(id) => format!("added live rule {id} for {words}"),
        None => format!("added a live rule for {words}"),
    };
    What {
        project: Some(project.to_owned()),
        target: id.map(|id| format!("rule {id}")),
        summary,
        ..What::default()
    }
}

/// A rule a person removes with `davy drop remove`.
pub struct Removed<'a> {
    pub project: &'a str,
    pub live: bool,
    /// The rule in the words `davy drop list` uses.
    pub words: &'a str,
}

/// Rule `id` removed with `davy drop remove`, and what it was when there
/// was one.
pub fn rule_removed(id: i64, removed: Option<Removed<'_>>) -> What {
    let target = Some(format!("rule {id}"));
    let Some(r) = removed else {
        return What {
            target,
            summary: format!("removed rule {id}"),
            ..What::default()
        };
    };
    let kind = if r.live { "live" } else { "shadow" };
    What {
        project: Some(r.project.to_owned()),
        target,
        summary: format!("removed {kind} rule {id} for {}", r.words),
        ..What::default()
    }
}

/// A change to an agent token.
pub fn token(change: &tokens::Change) -> What {
    let (name, summary) = match change {
        tokens::Change::Used { id, .. } => return said(&format!("used token {id}")),
        tokens::Change::Level { name, level } => (name, format!("set level {}", level.as_str())),
        tokens::Change::Unlock { name, .. } => (name, "unlocked destructive changes".to_owned()),
        tokens::Change::Lock { name } => (name, "locked destructive changes".to_owned()),
        tokens::Change::Revoke { name, .. } => (name, "revoked".to_owned()),
    };
    named(name, summary)
}

/// A token added at `level`.
pub fn token_added(name: &str, level: crate::access::Level) -> What {
    named(name, format!("added at level {}", level.as_str()))
}

fn named(name: &str, summary: String) -> What {
    What {
        target: Some(format!("token {name}")),
        summary,
        ..What::default()
    }
}

/// The action a row names for a token change.
pub fn token_action(change: &tokens::Change) -> &'static str {
    match change {
        tokens::Change::Used { .. } => "token_used",
        tokens::Change::Level { .. } => "token_level",
        tokens::Change::Unlock { .. } => "token_unlock",
        tokens::Change::Lock { .. } => "token_lock",
        tokens::Change::Revoke { .. } => "token_revoke",
    }
}

#[cfg(test)]
#[path = "describe_tests.rs"]
mod tests;
