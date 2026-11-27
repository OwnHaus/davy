//! `davy usage`: the `usage` tool's answer in a terminal. It opens the
//! database read-only, so it runs next to `davy serve`.

use super::{
    Request,
    forecast::WhatIf,
    report,
    savings::{self, View},
};
use crate::{query, retention, time};
use anyhow::Result;
use clap::builder::TypedValueParser;
use std::{io::Write, path::PathBuf};

#[derive(clap::Args)]
pub struct Args {
    /// The data directory of the server.
    #[arg(long)]
    pub data: PathBuf,
    /// Show one project's counts, and apply --logs-days to it alone.
    #[arg(long)]
    pub project: Option<String>,
    /// The window of the counts: 30m, 1h, 2d or an RFC 3339 timestamp, at
    /// most 90d back.
    #[arg(long, default_value = "7d")]
    pub since: String,
    /// What if the project, or every project, kept this many days of logs.
    #[arg(long)]
    pub logs_days: Option<u32>,
    /// What if all projects together added this many bytes of logs on
    /// disk a day. Logs of every project share one index per day, so this
    /// cannot be one project's.
    #[arg(long)]
    pub logs_bytes_per_day: Option<u64>,
    /// The server's default log retention, as given to `davy serve`, which
    /// this command cannot read from a running server.
    #[arg(long, default_value_t = retention::DEFAULT_LOG_DAYS)]
    pub log_retention_days: u32,
    /// The server's `--hot-days`, which the forecast needs for the days
    /// that turn cold.
    #[arg(long, default_value_t = retention::HotDays::default())]
    pub hot_days: retention::HotDays,
    /// summary: counts, disk and forecast. savings: where logs and errors
    /// could take less, with the command to try.
    #[arg(long, value_enum, default_value_t)]
    pub view: View,
    /// Entries of each kind in the savings view, 1 to 20.
    #[arg(
        long,
        default_value_t = savings::DEFAULT_TOP,
        value_parser = TypedValueParser::map(clap::value_parser!(u16).range(1..=20), usize::from),
    )]
    pub top: usize,
    /// Print JSON with sizes in bytes.
    #[arg(long)]
    pub json: bool,
}

pub fn run(out: &mut impl Write, args: &Args) -> Result<()> {
    let c = query::open(&args.data)?;
    let request = Request {
        project: args.project.clone(),
        since: Some(args.since.clone()),
        what_if: WhatIf {
            project: args.project.clone().filter(|_| args.logs_days.is_some()),
            logs_days: args.logs_days,
            logs_bytes_per_day: args.logs_bytes_per_day,
        },
        defaults: retention::Defaults {
            logs: retention::Days::new(args.log_retention_days).map_err(anyhow::Error::msg)?,
            hot_days: args.hot_days,
            ..retention::Defaults::default()
        },
    };
    let now = time::now_ns();
    let report = report(&c, &args.data, &request, now)?;
    if args.view == View::Savings {
        let ask = savings::Request {
            project: args.project.clone(),
            top: args.top,
            defaults: request.defaults,
        };
        let found = savings::savings(&c, &args.data, &report, &ask, now)?;
        if args.json {
            writeln!(out, "{}", crate::text::json(&found.json(false), false)?)?;
        } else {
            found.text(out)?;
        }
        return Ok(());
    }
    if args.json {
        writeln!(out, "{}", crate::text::json(&report.json(false), false)?)?;
    } else {
        report.text(out)?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "cli_tests.rs"]
mod tests;
