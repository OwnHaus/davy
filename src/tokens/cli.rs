//! `davy token`: the agent tokens of `/mcp`. Writes from its own process,
//! like `davy user`. `add` opens the store as `davy project` does, `list`
//! reads it read-only, and the changes open it without creating or
//! upgrading it, as `davy drop live` does. Whoever runs it on the host
//! counts as a person, so it may unlock destructive changes for a token, as
//! the web UI does. Every change leaves a row in the ledger.

use super::{Change, Listed, Name, apply, create, list, one};
use crate::{
    access::Level,
    ledger::{self, Entry, Who, describe},
    time,
};
use anyhow::Context as _;
use clap::Subcommand;
use std::{io::Write, path::PathBuf};

/// Who `davy token unlock` records as the one who unlocked.
pub const BY: &str = ledger::CLI;

#[derive(Subcommand)]
pub enum Token {
    /// Adds a token and prints its secret, once, as JSON. Give it to one
    /// agent: as DAVY_TOKEN where its MCP client starts, or as the bearer
    /// token of its MCP config. davy serve refuses it as its own token.
    Add {
        /// 1 to 64 of a-z, 0-9, '.', '_' and '-'.
        name: String,
        /// read, triage or full. See "Agent tokens" in docs/design.md.
        #[arg(long)]
        level: String,
        #[arg(long)]
        data: PathBuf,
    },
    /// Lists the tokens, live ones first, never their secrets.
    List {
        #[arg(long)]
        data: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Revokes a token: its next call answers 401.
    Revoke {
        /// The token to revoke.
        name: String,
        #[arg(long)]
        data: PathBuf,
    },
    /// Changes a token's level. Below full it loses its unlock.
    Level {
        /// The token to change.
        name: String,
        /// read, triage or full.
        level: String,
        #[arg(long)]
        data: PathBuf,
    },
    /// Unlocks destructive changes for a token of level full.
    Unlock {
        /// The token to unlock, of level full.
        name: String,
        #[arg(long)]
        data: PathBuf,
    },
    /// Locks destructive changes for a token again.
    Lock {
        /// The token to lock.
        name: String,
        #[arg(long)]
        data: PathBuf,
    },
}

pub fn run(out: &mut impl Write, command: Token) -> anyhow::Result<()> {
    let now = time::now_ms();
    let (data, name, change) = match command {
        Token::Add { name, level, data } => {
            let name = Name::parse(&name).map_err(anyhow::Error::msg)?;
            let level = Level::parse(&level).map_err(anyhow::Error::msg)?;
            let c = crate::store::open(&data)?;
            let what = describe::token_added(name.as_str(), level);
            let entry = Entry::of(Who::Cli, "token_add", what);
            let (token, secret) = ledger::recorded(&c, entry, |c| create(c, &name, level, now))?
                .map_err(anyhow::Error::msg)?;
            let v = serde_json::json!({ "token": token, "secret": secret });
            writeln!(out, "{v}")?;
            return Ok(());
        }
        Token::List { data, json } => return print(out, &list(&crate::query::open(&data)?)?, json),
        Token::Revoke { name, data } => (data, name.clone(), Change::Revoke { name, at: now }),
        Token::Level { name, level, data } => {
            let level = Level::parse(&level).map_err(anyhow::Error::msg)?;
            (data, name.clone(), Change::Level { name, level })
        }
        Token::Unlock { name, data } => {
            let by = BY.to_owned();
            (data, name.clone(), Change::Unlock { name, by, at: now })
        }
        Token::Lock { name, data } => (data, name.clone(), Change::Lock { name }),
    };
    let c = crate::store::open_current(&data)?;
    let entry = Entry::of(
        Who::Cli,
        describe::token_action(&change),
        describe::token(&change),
    );
    ledger::recorded(&c, entry, |c| apply(c, &change))?.map_err(anyhow::Error::msg)?;
    let token = one(&c, &name)?.context("token vanished")?;
    writeln!(out, "{}", serde_json::json!({ "token": token }))?;
    Ok(())
}

/// One token a line: name, level, `destructive` or `-`, last use or
/// `never`, and `revoked` for a revoked one.
fn print(out: &mut impl Write, tokens: &[Listed], json: bool) -> anyhow::Result<()> {
    if json {
        writeln!(out, "{}", serde_json::to_string(tokens)?)?;
        return Ok(());
    }
    for t in tokens {
        let unlocked = if t.destructive { "destructive" } else { "-" };
        let used = t.used_at.as_deref().unwrap_or("never");
        let revoked = if t.revoked_at.is_some() {
            "\trevoked"
        } else {
            ""
        };
        writeln!(
            out,
            "{}\t{}\t{unlocked}\t{used}{revoked}",
            t.name,
            t.level.as_str()
        )?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "cli_tests.rs"]
mod tests;
