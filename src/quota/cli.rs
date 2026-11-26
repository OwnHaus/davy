//! `davy project quota`: shows or sets a project's daily quotas. A running
//! server reads the change within [`super::RELOAD_EVERY`].

use super::{Settings, settings, status};
use crate::{store, time};
use clap::Parser;
use std::{io::Write, path::PathBuf};

/// The largest log quota, 1 PB: more than any disk Davy runs on.
const MAX_SIZE: u64 = 1_000_000_000_000_000;

#[derive(Parser)]
pub struct Args {
    /// Project slug.
    pub slug: String,
    #[arg(long)]
    pub data: PathBuf,
    /// Error events a UTC day, or `off` for no quota.
    #[arg(long, value_name = "N", value_parser = count, allow_hyphen_values = true)]
    pub errors: Option<Limit>,
    /// Log bytes a UTC day as received, decompressed, with a unit: B, KB,
    /// MB, GB or TB in powers of 1000, such as 2GB or 500MB, at most 1 PB,
    /// or `off` for no quota.
    #[arg(long, value_name = "SIZE", value_parser = size, allow_hyphen_values = true)]
    pub logs: Option<Limit>,
    /// The percent of a quota at which an alert warns, 1 to 100.
    #[arg(long, value_name = "PERCENT", value_parser = clap::value_parser!(u8).range(1..=100))]
    pub warn: Option<u8>,
    /// Removes every quota and the warning percent.
    #[arg(long, conflicts_with_all = ["errors", "logs", "warn"])]
    pub clear: bool,
    /// Prints the same as JSON, numbers only.
    #[arg(long)]
    pub json: bool,
}

/// A quota, or `None` for none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limit(pub Option<u64>);

/// Applies `args` and prints the project's quotas and today's use.
pub fn run(out: &mut impl Write, args: &Args) -> anyhow::Result<()> {
    let c = store::open(&args.data)?;
    let id = store::project_by_slug(&c, &args.slug)?;
    let mut s = settings::of(&c, id)?;
    if args.clear {
        s = Settings::default();
    }
    if let Some(Limit(n)) = args.errors {
        s.errors = n;
    }
    if let Some(Limit(n)) = args.logs {
        s.logs = n;
    }
    if let Some(warn) = args.warn {
        s.warn = warn;
    }
    if s != settings::of(&c, id)? {
        settings::store(&c, id, &s)?;
    }
    let now = time::now_ns();
    let status = status::of(&c, id, now)?;
    if args.json {
        writeln!(out, "{}", status.json(now))?;
    } else {
        write!(
            out,
            "project: {}\n{}A running server applies a change within {} s.\n",
            args.slug,
            status.text(now),
            super::RELOAD_EVERY.as_secs(),
        )?;
    }
    Ok(())
}

/// A positive whole number, or `off`.
pub fn count(s: &str) -> Result<Limit, String> {
    if s == "off" {
        return Ok(Limit(None));
    }
    match s.parse::<u64>() {
        Ok(n) if n > 0 && i64::try_from(n).is_ok() => Ok(Limit(Some(n))),
        _ => Err(format!("expected a whole number over 0 or off, got {s:?}")),
    }
}

/// A size with its unit such as `2GB`, `1.5 GB`, `500MB` or `1000B`, in
/// powers of 1000 as Davy prints sizes, at most [`MAX_SIZE`], or `off`.
/// A bare number is refused, as `--logs 2` meaning 2 bytes is a typo.
pub fn size(s: &str) -> Result<Limit, String> {
    if s == "off" {
        return Ok(Limit(None));
    }
    let bad = || {
        format!(
            "expected a size over 0 with a unit, B, KB, MB, GB or TB in powers of 1000, \
             such as 2GB or 500MB, at most 1 PB, or off, got {s:?}"
        )
    };
    let t = s.trim();
    let split = t
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(t.len());
    let (number, unit) = t.split_at(split);
    let scale: u64 = match unit.trim().to_ascii_uppercase().as_str() {
        "B" => 1,
        "KB" => 1_000,
        "MB" => 1_000_000,
        "GB" => 1_000_000_000,
        "TB" => 1_000_000_000_000,
        _ => return Err(bad()),
    };
    let (whole, tenths) = decimal(number).ok_or_else(bad)?;
    let bytes = whole
        .checked_mul(scale)
        .and_then(|b| b.checked_add(tenths.checked_mul(scale)? / 1_000_000))
        .filter(|b| (1..=MAX_SIZE).contains(b))
        .ok_or_else(bad)?;
    Ok(Limit(Some(bytes)))
}

/// `n` as its whole part and its fraction in millionths, at most six
/// digits after the point.
fn decimal(n: &str) -> Option<(u64, u64)> {
    let (whole, fraction) = n.split_once('.').unwrap_or((n, ""));
    if whole.is_empty() || fraction.len() > 6 || !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let millionths = format!("{fraction:0<6}").parse().ok()?;
    Some((whole.parse().ok()?, millionths))
}
