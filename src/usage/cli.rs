//! `davy usage`: the `usage` tool's answer in a terminal. It opens the
//! database read-only, so it runs next to `davy serve`.

use super::{Request, forecast::WhatIf, report};
use crate::{query, retention, time};
use anyhow::Result;
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
            ..retention::Defaults::default()
        },
    };
    let report = report(&c, &args.data, &request, time::now_ns())?;
    if args.json {
        writeln!(out, "{}", report.json(false))?;
    } else {
        report.text(out)?;
    }
    Ok(())
}
