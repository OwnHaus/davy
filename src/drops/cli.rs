//! `davy drop`: the only way to write a live rule. Prints JSON after a
//! change, and `list` prints text or JSON. `live` shows the rule and asks
//! first.

use super::{By, Match, Spec, Stored, counts::Counted};
use crate::{
    install::sh,
    logs::patterns,
    store,
    text::{self, printable},
    time,
    usage::{Count, human},
};
use anyhow::{Result, anyhow};
use jiff::ToSpan as _;
use rusqlite::{Connection, TransactionBehavior};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::{BufRead, Write},
    path::{Path, PathBuf},
};

/// Days `list` counts back, today included.
pub const LIST_DAYS: i64 = 7;

/// `davy drop`.
#[derive(clap::Subcommand)]
pub enum Drop {
    /// Adds a rule after the project's others. A live rule drops the lines
    /// it names before they are stored, and the first live rule that
    /// matches a line decides; `--shadow` drops nothing and counts what
    /// the rule would drop. Name exactly one of --pattern, --level and
    /// --message. Adding a rule that matches like one the project has
    /// prints that one. Prints the rule as JSON.
    Add {
        /// A pattern id from `davy usage --view savings` or `davy patterns`.
        #[arg(long)]
        pattern: Option<String>,
        /// Lines of this level and every lower one, such as `debug`.
        #[arg(long)]
        level: Option<String>,
        /// Lines whose message holds this text, case and all. With `*` it
        /// matches the whole message, each `*` any text.
        #[arg(long)]
        message: Option<String>,
        /// Only lines of this service, exactly.
        #[arg(long)]
        service: Option<String>,
        /// Keep this share of the lines, such as `1%`: the first, then one
        /// in every 100. Leave it out to drop all.
        #[arg(long)]
        keep: Option<String>,
        /// Drop nothing and count what the rule would drop.
        #[arg(long)]
        shadow: bool,
        /// With --pattern, match warnings, errors and fatal lines of the
        /// pattern too. Without it a pattern rule skips them.
        #[arg(long)]
        any_level: bool,
        #[arg(long)]
        data: PathBuf,
        /// The project's slug.
        project: String,
    },
    /// Lists the rules in the order the log writer tries them, with what
    /// each dropped or would drop in the last 7 days.
    List {
        #[arg(long)]
        data: PathBuf,
        #[arg(long)]
        json: bool,
        /// Only the rules of this project.
        project: Option<String>,
    },
    /// Makes a rule live, after the project's other live rules. A person
    /// holds it from then on and it never expires. Shows the rule and what
    /// it would have dropped in the last 7 days, then asks.
    Live {
        /// The ID that `davy drop list` prints.
        rule_id: i64,
        /// Do not ask, for a script. Without a terminal to ask on, `live`
        /// refuses unless this is given.
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        data: PathBuf,
    },
    /// Makes a rule a shadow rule again: it stops dropping and counts.
    Shadow {
        /// The ID that `davy drop list` prints.
        rule_id: i64,
        #[arg(long)]
        data: PathBuf,
    },
    /// Removes a rule. What it dropped stays counted in `davy usage`.
    Remove {
        /// The ID that `davy drop list` prints.
        rule_id: i64,
        #[arg(long)]
        data: PathBuf,
    },
}

/// A rule as `davy drop list` and the `drop_rules` tool show it.
pub struct Listed {
    pub rule: Stored,
    pub project: String,
    pub counted: Counted,
    /// The data directory, quoted for a shell, for the command shown.
    pub data: String,
}

impl Listed {
    /// The command a person runs to make this rule live.
    pub fn live_command(&self) -> String {
        format!("davy drop live --data {} {}", self.data, self.rule.id)
    }

    /// Sizes as [`human`] strings, or with `human` false as bytes.
    pub fn json(&self, human_sizes: bool) -> Value {
        let size = |b: u64| {
            if human_sizes {
                json!(human(b))
            } else {
                json!(b)
            }
        };
        let count = |n: Count| json!({"records": n.records, "bytes": size(n.bytes)});
        let r = &self.rule;
        let (pattern, level, message) = match &r.spec.matches {
            Match::Pattern(id) => (Some(patterns::show(*id)), None, None),
            Match::Level(l) => (None, Some(l.as_str()), None),
            Match::Message(m) => (None, None, Some(m.as_str())),
        };
        let mut out = json!({
            "id": r.id,
            "project": self.project,
            "state": if r.live { "live" } else { "shadow" },
            "by": r.by.as_str(),
            "position": r.position,
            "pattern": pattern,
            "level": level,
            "message": message,
            "service": r.spec.service,
            "keep_percent": r.spec.keep,
            "created_at": time::rfc3339_ms(r.created_at),
            "expires_at": r.expires_at.map(time::rfc3339_ms),
            "last_7_days": {
                "dropped": count(self.counted.dropped),
                "would_drop": count(self.counted.would_drop),
            },
        });
        if !r.live {
            out["make_live"] = json!(format!("a person can run: {}", self.live_command()));
        }
        out
    }

    /// One line of `davy drop list`, the text from outside made printable.
    fn line(&self) -> String {
        let r = &self.rule;
        let counted =
            |verb: &str, n: Count| format!("{verb} {} lines ({})", n.records, human(n.bytes));
        let counted = if r.live {
            counted("dropped", self.counted.dropped)
        } else {
            counted("would drop", self.counted.would_drop)
        };
        let expires = r.expires_at.map_or_else(String::new, |at| {
            format!(", expires {}", time::rfc3339_ms(at))
        });
        let command = if r.live {
            String::new()
        } else {
            format!("; a person can run: {}", self.live_command())
        };
        format!(
            "{:<5} {:<20} {:<6} {:<6} {}: {counted} in {LIST_DAYS} days{expires}{command}",
            r.id,
            printable(&self.project, char::REPLACEMENT_CHARACTER),
            if r.live { "live" } else { "shadow" },
            r.by.as_str(),
            describe(&r.spec),
        )
    }
}

/// What `spec` names and keeps, as `davy drop list` prints it: text from
/// outside quoted, with quotes and backslashes escaped and what a terminal
/// would act on made printable.
pub fn describe(spec: &Spec) -> String {
    let what = match &spec.matches {
        Match::Pattern(id) if spec.any_level => {
            format!("pattern {} at any level", patterns::show(*id))
        }
        Match::Pattern(id) => format!("pattern {} below warn", patterns::show(*id)),
        Match::Level(l) => format!("level {} and below", l.as_str()),
        Match::Message(m) if m.contains('*') => format!("message like {}", quoted(m)),
        Match::Message(m) => format!("message contains {}", quoted(m)),
    };
    let service = spec
        .service
        .as_deref()
        .map_or_else(String::new, |s| format!(" service {}", quoted(s)));
    let keep = match spec.keep {
        0 => "drop all".to_owned(),
        n => format!("keep {n}%"),
    };
    format!("{what}{service}, {keep}")
}

/// `s` in double quotes, `"` and `\` escaped with a backslash.
fn quoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in printable(s, char::REPLACEMENT_CHARACTER).chars() {
        if matches!(c, '"' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// The rules in force at `now_ms` of `project` or of all, with what each
/// counted in the last [`LIST_DAYS`] days.
pub fn list(c: &Connection, project: Option<i64>, data: &Path, now_ms: i64) -> Result<Vec<Listed>> {
    let rules = super::load(c, project, now_ms)?;
    let today = time::day_of(now_ms.saturating_mul(1_000_000));
    let from = today.checked_sub((LIST_DAYS - 1).days())?;
    let counted = super::counts::by_rule(c, from, project)?;
    let slugs: HashMap<i64, String> = c
        .prepare("SELECT id, slug FROM projects")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    // Absolute, so the command shown works from any directory.
    let data = std::path::absolute(data).unwrap_or_else(|_| data.to_owned());
    let data = sh(&data.display().to_string());
    Ok(rules
        .into_iter()
        .map(|rule| Listed {
            project: slugs.get(&rule.project).cloned().unwrap_or_default(),
            counted: counted.get(&rule.id).copied().unwrap_or_default(),
            data: data.clone(),
            rule,
        })
        .collect())
}

/// Rule `id` as listed, after a change.
fn one(c: &Connection, id: i64, data: &Path) -> Result<Listed> {
    let rule = super::get(c, id)?.ok_or_else(|| anyhow!("no rule {id}"))?;
    list(c, Some(rule.project), data, time::now_ms())?
        .into_iter()
        .find(|l| l.rule.id == id)
        .ok_or_else(|| anyhow!("rule {id} expired"))
}

/// Runs `command`, printing its JSON or list to `out` and what `live`
/// shows before it asks to `err`. `answer` is the terminal `live` reads a
/// yes from, or None when there is none.
pub fn run(
    out: &mut impl Write,
    err: &mut impl Write,
    answer: Option<&mut dyn BufRead>,
    command: Drop,
) -> Result<()> {
    match command {
        Drop::Add {
            pattern,
            level,
            message,
            service,
            keep,
            shadow,
            any_level,
            data,
            project,
        } => {
            let mut spec = Spec::new(
                pattern.as_deref(),
                level.as_deref(),
                message.as_deref(),
                service.as_deref(),
                keep.as_deref(),
            )
            .map_err(anyhow::Error::msg)?;
            if any_level && pattern.is_none() {
                anyhow::bail!("--any-level is for a --pattern rule only");
            }
            spec.any_level = any_level;
            add(out, &spec, !shadow, &data, &project)?;
        }
        Drop::List {
            data,
            json,
            project,
        } => {
            let c = crate::query::open(&data)?;
            let id = project
                .map(|slug| store::project_by_slug(&c, &slug))
                .transpose()?;
            print_list(out, &list(&c, id, &data, time::now_ms())?, json)?;
        }
        Drop::Live { rule_id, yes, data } => {
            let c = store::open_current(&data)?;
            if !yes {
                confirm(err, answer, &one(&c, rule_id, &data)?)?;
            }
            toggle(out, &c, rule_id, &data, true)?;
        }
        Drop::Shadow { rule_id, data } => {
            toggle(out, &store::open_current(&data)?, rule_id, &data, false)?;
        }
        Drop::Remove { rule_id, data } => {
            if !super::remove(&store::open_current(&data)?, rule_id)? {
                anyhow::bail!("no rule {rule_id}");
            }
            writeln!(out, "{}", text::json(&json!({"removed": rule_id}), false)?)?;
        }
    }
    Ok(())
}

/// Adds `spec` to `project` as a person and prints the rule.
fn add(out: &mut impl Write, spec: &Spec, live: bool, data: &Path, project: &str) -> Result<()> {
    let mut c = store::open(data)?;
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let id = store::project_by_slug(&tx, project)?;
    let added =
        super::add(&tx, id, spec, live, By::Person, time::now_ms())?.map_err(anyhow::Error::msg)?;
    tx.commit()?;
    let mut shown = one(&c, added.id, data)?.json(false);
    shown["created"] = json!(added.created);
    writeln!(out, "{}", text::json(&shown, false)?)?;
    Ok(())
}

/// Shows what `rule` names and what it would have dropped in the last
/// [`LIST_DAYS`] days, then asks on `answer`. An error unless the answer
/// is yes, and when there is no terminal to ask on.
fn confirm(err: &mut impl Write, answer: Option<&mut dyn BufRead>, rule: &Listed) -> Result<()> {
    let r = &rule.rule;
    if r.live {
        return Ok(());
    }
    let n = rule.counted.would_drop;
    writeln!(
        err,
        "rule {} of {}: {}",
        r.id,
        printable(&rule.project, char::REPLACEMENT_CHARACTER),
        describe(&r.spec)
    )?;
    writeln!(
        err,
        "in the last {LIST_DAYS} days it would have dropped {} lines ({})",
        n.records,
        human(n.bytes)
    )?;
    if r.spec.hides_warnings() {
        writeln!(err, "it can drop warnings and errors too")?;
    }
    let Some(answer) = answer else {
        anyhow::bail!(
            "davy drop live asks before a rule drops lines, and there is no terminal to ask on: add --yes to make rule {} live",
            r.id
        );
    };
    write!(
        err,
        "make it live, so these lines are dropped from now on? [y/N] "
    )?;
    err.flush()?;
    let mut line = String::new();
    answer.read_line(&mut line)?;
    if !matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        anyhow::bail!("rule {} stays a shadow rule", r.id);
    }
    Ok(())
}

/// Makes rule `id` live, or with `live` false a shadow rule, and prints it.
fn toggle(out: &mut impl Write, c: &Connection, id: i64, data: &Path, live: bool) -> Result<()> {
    if !super::set_live(c, id, live, time::now_ms())? {
        anyhow::bail!("no rule {id}, or it expired");
    }
    writeln!(
        out,
        "{}",
        text::json(&one(c, id, data)?.json(false), false)?
    )?;
    Ok(())
}

fn print_list(out: &mut impl Write, rules: &[Listed], json: bool) -> Result<()> {
    if json {
        let all: Vec<Value> = rules.iter().map(|l| l.json(false)).collect();
        writeln!(out, "{}", text::json(&all, false)?)?;
    } else if rules.is_empty() {
        writeln!(out, "no drop rules")?;
    } else {
        for l in rules {
            writeln!(out, "{}", l.line())?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "cli_tests.rs"]
mod tests;
