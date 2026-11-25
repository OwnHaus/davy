//! The Vercel Log Drain setting of a project: the secret Vercel signs each
//! drain request with. The server checks `x-vercel-signature` only for a
//! project that has one (`src/http/vercel.rs`).
//!
//! The secret is kept as Vercel shows it, because checking an HMAC needs the
//! key itself, as for the signing secret of an alert channel. It reaches
//! Davy only on standard input, never on a command line, and the SQL tool
//! reads it as NULL.

use crate::store;
use anyhow::{Context as _, bail, ensure};
use rusqlite::{Connection, OptionalExtension as _};
use serde_json::json;
use std::{
    io::{Read, Write},
    path::PathBuf,
};

/// The schema change that adds the setting.
pub const MIGRATION: &str = "ALTER TABLE projects ADD COLUMN vercel_secret TEXT;";
/// What takes a store back to before [`MIGRATION`], for migration tests.
pub const UNDO: &str = "ALTER TABLE projects DROP COLUMN vercel_secret;";

/// Longer than any secret Vercel makes, short enough that a file read by
/// mistake is refused.
const MAX_SECRET: usize = 1024;

/// The drain secret of `project`, if it has one.
pub fn secret(c: &Connection, project: i64) -> anyhow::Result<Option<String>> {
    Ok(c.query_row(
        "SELECT vercel_secret FROM projects WHERE id=?1",
        [project],
        |r| r.get::<_, Option<String>>(0),
    )
    .optional()?
    .flatten())
}

fn set(c: &Connection, project: i64, secret: Option<&str>) -> anyhow::Result<()> {
    c.execute(
        "UPDATE projects SET vercel_secret=?1 WHERE id=?2",
        rusqlite::params![secret, project],
    )?;
    Ok(())
}

#[derive(clap::Args)]
pub struct Args {
    /// Project slug to configure.
    pub slug: String,
    #[arg(long)]
    pub data: PathBuf,
    /// Read the drain's signature verification secret from standard input
    /// and refuse drain requests not signed with it.
    #[arg(long, conflicts_with = "no_secret")]
    pub secret_stdin: bool,
    /// Forget the secret: the project key alone lets a drain request in.
    #[arg(long)]
    pub no_secret: bool,
}

/// Sets or clears the secret as `args` say, reading it from `stdin`, and
/// prints whether drain requests to the project must be signed. Never
/// prints the secret.
pub fn run(out: &mut impl Write, args: &Args, stdin: impl Read) -> anyhow::Result<()> {
    let c = store::open(&args.data)?;
    let id = store::project_by_slug(&c, &args.slug)?;
    if args.secret_stdin {
        set(&c, id, Some(&read_secret(stdin)?))?;
    } else if args.no_secret {
        set(&c, id, None)?;
    }
    let required = secret(&c, id)?.is_some();
    writeln!(
        out,
        "{}",
        json!({"project": args.slug, "signature_required": required})
    )?;
    Ok(())
}

/// One line of at most [`MAX_SECRET`] bytes, without the spaces and line
/// break around it, which a paste or `echo` adds and Vercel's secret does
/// not have.
fn read_secret(stdin: impl Read) -> anyhow::Result<String> {
    // Room for the secret and as much space again, plus one byte that
    // tells a longer input from one that fits.
    let room = 2 * MAX_SECRET;
    let mut bytes = Vec::new();
    stdin
        .take(room as u64 + 1)
        .read_to_end(&mut bytes)
        .context("could not read the secret from standard input")?;
    let value = bytes.trim_ascii();
    ensure!(
        !value.is_empty(),
        "no secret on standard input, pipe in the drain's signature verification secret"
    );
    if bytes.len() > room || value.len() > MAX_SECRET {
        bail!("the secret is longer than {MAX_SECRET} bytes");
    }
    let value =
        std::str::from_utf8(value).context("the secret on standard input is not UTF-8 text")?;
    if value.chars().any(char::is_control) {
        bail!("the secret is one line of text, without control characters");
    }
    Ok(value.to_owned())
}

#[cfg(test)]
#[path = "vercel_tests.rs"]
mod tests;
