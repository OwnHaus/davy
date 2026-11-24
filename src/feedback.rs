//! What agents report about Davy itself with the `submit_feedback` MCP tool.
//! It stays in the data directory unless a channel was added with
//! `--feedback`.

use crate::{alert, event::Timestamp, text, time};
use anyhow::Result;
use rusqlite::{Connection, params};
use serde::Serialize;
use std::{borrow::Cow, io::Write, path::PathBuf};

/// Enough for a command, its output and a paragraph of explanation.
pub const MAX_CHARS: usize = 4000;
const MAX_CLIENT_CHARS: usize = 100;
/// The newest rows kept, so an agent in a loop cannot fill the disk.
const KEEP: i64 = 1000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Feedback {
    /// The MCP client, such as `claude-code`, when it said its name.
    pub client: Option<String>,
    pub tried: String,
    pub happened: String,
    pub fix: Option<String>,
}

impl Feedback {
    /// Checks what an agent sent. The client name is cut rather than refused,
    /// because the agent does not choose it.
    pub fn new(
        client: Option<&str>,
        tried: String,
        happened: String,
        fix: Option<String>,
    ) -> Result<Self, String> {
        let fix = fix.filter(|f| !f.trim().is_empty());
        for (name, text) in [("tried", Some(&tried)), ("happened", Some(&happened))]
            .into_iter()
            .chain([("fix", fix.as_ref())])
        {
            let Some(text) = text else { continue };
            if text.trim().is_empty() {
                return Err(format!("{name} is empty"));
            }
            let chars = text.chars().count();
            if chars > MAX_CHARS {
                return Err(format!(
                    "{name} holds {chars} characters and may hold {MAX_CHARS}, shorten it"
                ));
            }
        }
        let client = client
            .map(|c| {
                c.chars()
                    .filter(|c| !c.is_control())
                    .take(MAX_CLIENT_CHARS)
                    .collect::<String>()
            })
            .filter(|c| !c.trim().is_empty());
        let mut feedback = Self {
            client,
            tried,
            happened,
            fix,
        };
        let policy = crate::scrub::Policy::default();
        policy.text(&mut feedback.tried);
        policy.text(&mut feedback.happened);
        for text in [&mut feedback.client, &mut feedback.fix]
            .into_iter()
            .flatten()
        {
            policy.text(text);
        }
        Ok(feedback)
    }
}

/// Stores `feedback`, drops rows older than the newest [`KEEP`], and queues a
/// notice for each channel that takes feedback. Returns the row ID.
pub fn add(c: &Connection, feedback: &Feedback, at: Timestamp) -> Result<i64> {
    c.execute(
        "INSERT INTO feedback(at,client,tried,happened,fix) VALUES (?1,?2,?3,?4,?5)",
        params![
            at,
            feedback.client,
            feedback.tried,
            feedback.happened,
            feedback.fix
        ],
    )?;
    let id = c.last_insert_rowid();
    c.execute("DELETE FROM feedback WHERE id<=?1", [id - KEEP])?;
    alert::store::notify_feedback(
        c,
        &alert::Notice::Feedback {
            id,
            feedback: feedback.clone(),
        },
        at,
    )?;
    Ok(id)
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Listed {
    pub id: i64,
    /// RFC 3339 in UTC.
    pub at: String,
    #[serde(flatten)]
    pub feedback: Feedback,
}

/// Every stored item, oldest first.
pub fn list(c: &Connection) -> Result<Vec<Listed>> {
    let mut q = c.prepare("SELECT id,at,client,tried,happened,fix FROM feedback ORDER BY id")?;
    let rows = q.query_map([], |r| {
        Ok(Listed {
            id: r.get(0)?,
            at: time::rfc3339_ms(r.get(1)?),
            feedback: Feedback {
                client: r.get(2)?,
                tried: r.get(3)?,
                happened: r.get(4)?,
                fix: r.get(5)?,
            },
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[derive(clap::Args)]
pub struct Args {
    #[arg(long)]
    pub data: PathBuf,
    #[arg(long)]
    pub json: bool,
}

pub fn run(out: &mut impl Write, args: &Args) -> Result<()> {
    let items = list(&crate::store::open(&args.data)?)?;
    if args.json {
        writeln!(out, "{}", serde_json::to_string(&items)?)?;
        return Ok(());
    }
    for item in items {
        let f = &item.feedback;
        writeln!(
            out,
            "{}\t{}\t{}",
            item.id,
            item.at,
            f.client.as_deref().map_or(Cow::Borrowed("-"), one_line)
        )?;
        writeln!(out, "  tried: {}", one_line(&f.tried))?;
        writeln!(out, "  happened: {}", one_line(&f.happened))?;
        if let Some(fix) = &f.fix {
            writeln!(out, "  fix: {}", one_line(fix))?;
        }
    }
    Ok(())
}

/// The text on one line with no control characters, so an agent's text
/// cannot move the cursor or rewrite earlier lines in a terminal.
fn one_line(s: &str) -> Cow<'_, str> {
    text::printable(s, ' ')
}

#[cfg(test)]
#[path = "feedback_tests.rs"]
mod tests;
