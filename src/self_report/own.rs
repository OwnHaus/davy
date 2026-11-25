//! Davy's own project. It is marked in `projects.own`, not found by its
//! slug, so a project a person named `davy` stays theirs: Davy's own
//! project then takes the next free slug of [`slugs`].

use super::{LOG_DAYS, PROJECT, info, set_project_id, stderr};
use crate::{scrub::Policy, store};
use rusqlite::{Connection, OptionalExtension as _, TransactionBehavior, params};
use serde::Serialize;
use std::path::Path;

/// The schema change that marks Davy's own project. The index allows one.
pub const MIGRATION: &str = "ALTER TABLE projects ADD COLUMN own INTEGER NOT NULL DEFAULT 0;
CREATE UNIQUE INDEX projects_own ON projects(own) WHERE own=1;";
/// What takes a store back to before [`MIGRATION`], for migration tests.
pub const UNDO: &str = "DROP INDEX projects_own; ALTER TABLE projects DROP COLUMN own;";
/// The slugs tried after [`PROJECT`] and `davy-self` before Davy gives up.
const MAX_TRIES: u32 = 100;
const COMPONENT: &str = "self report";
/// The SQL condition true for the rows of a project a person made, with
/// `project_id` the column that names the project.
pub const NOT_OWN: &str = "project_id NOT IN (SELECT id FROM projects WHERE own=1)";

/// Davy's own project.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Own {
    pub id: i64,
    pub slug: String,
}

/// Opens the store in `data` and finds or makes Davy's own project.
/// `davy serve` calls it before its writers start. A project made now is
/// said in an info line, which is recorded in it. None, said on stderr,
/// when the store cannot be opened or the project made: Davy then runs
/// without recording itself. The connection is the forwarder's, to read
/// the project's policy and events with.
pub fn open(data: &Path) -> Option<(Own, Connection)> {
    let opened = store::open(data).and_then(|mut c| Ok((find_or_create(&mut c)?, c)));
    match opened {
        Ok(((own, created), c)) => {
            set_project_id(own.id);
            if created {
                info(COMPONENT, created_line(&own.slug));
            }
            Some((own, c))
        }
        Err(e) => {
            stderr(
                COMPONENT,
                format!(
                    "Davy's own lines and crashes are not recorded, as its project could not be opened: {e:#}"
                ),
            );
            None
        }
    }
}

/// The line that says the project was made, and how to stop it.
pub fn created_line(slug: &str) -> String {
    let taken = if slug == PROJECT {
        String::new()
    } else {
        format!(" (a project of a person has the slug {PROJECT:?})")
    };
    format!(
        "created project {slug:?}{taken} for Davy's own warnings, errors and crashes, keeping {LOG_DAYS} days of logs; `davy serve --no-self-report` turns this off"
    )
}

/// Davy's own project, if it was made.
pub fn find(c: &Connection) -> anyhow::Result<Option<Own>> {
    Ok(
        c.query_row("SELECT id, slug FROM projects WHERE own=1", [], |r| {
            Ok(Own {
                id: r.get(0)?,
                slug: r.get(1)?,
            })
        })
        .optional()?,
    )
}

/// Finds Davy's own project, or makes it with [`LOG_DAYS`] of logs and
/// [`policy`] under the first free slug. True when it was made. `davy
/// serve` calls this before its writers start, so the store has one writer
/// at a time.
pub fn find_or_create(c: &mut Connection) -> anyhow::Result<(Own, bool)> {
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(own) = find(&tx)? {
        return Ok((own, false));
    }
    let slug = free_slug(&tx)?;
    let (id, _key) = store::create_project(&tx, &slug)?;
    tx.execute(
        "UPDATE projects SET own=1, log_retention_days=?1 WHERE id=?2",
        params![LOG_DAYS, id],
    )?;
    crate::scrub::settings::set(&tx, id, &policy())?;
    tx.commit()?;
    Ok((Own { id, slug }, true))
}

/// The slugs Davy's own project may take, in order: `davy`, `davy-self`,
/// `davy-self-2` and so on.
pub fn slugs() -> impl Iterator<Item = String> {
    [PROJECT.to_owned(), format!("{PROJECT}-self")]
        .into_iter()
        .chain((2..=MAX_TRIES).map(|n| format!("{PROJECT}-self-{n}")))
}

fn free_slug(c: &Connection) -> anyhow::Result<String> {
    for slug in slugs() {
        if store::project_id(c, &slug)?.is_none() {
            return Ok(slug);
        }
    }
    anyhow::bail!("every slug from {PROJECT:?} to \"{PROJECT}-self-{MAX_TRIES}\" is taken")
}

/// The policy of Davy's own project: the default, and also e-mail
/// addresses and personal data, which an alert or a check may name and no
/// operator needs from Davy's own lines.
pub fn policy() -> Policy {
    Policy {
        email: true,
        personal: true,
        ..Policy::default()
    }
}

#[cfg(test)]
#[path = "own_tests.rs"]
mod tests;
