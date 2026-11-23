//! `davy monitor`: uptime checks of a URL and heartbeats from cron jobs.
//! Writes from its own process, like the other commands that change the
//! database.

use super::{CheckUrl, Expect, NewMonitor, Target, checkin::MonitorSlug, edit::Edit, store};
use crate::{text, time};
use clap::{ArgGroup, Args, Subcommand};
use std::{borrow::Cow, io::Write, path::PathBuf, time::Duration};

#[derive(Subcommand)]
pub enum Monitor {
    /// Adds an HTTP check with --url, or a heartbeat with --heartbeat. Prints
    /// the monitor's ID and its URL or ping path.
    Add(Add),
    /// Prints each monitor with its state, last check and open incident.
    List {
        #[arg(long)]
        data: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Changes a monitor in place: only the given flags change, and it keeps
    /// its ID, checks, incidents and heartbeat token. Prints what add prints.
    Edit(EditArgs),
    /// Deletes a monitor with its checks and incidents.
    Remove {
        /// The ID that `davy monitor list` prints.
        id: i64,
        #[arg(long)]
        data: PathBuf,
    },
}

#[derive(Args)]
#[command(group(ArgGroup::new("target").required(true).args(["url", "heartbeat"])))]
pub struct Add {
    /// The project slug whose channels get the alerts.
    project: String,
    /// A name for alerts and lists, such as `api health`.
    name: String,
    #[arg(long)]
    data: PathBuf,
    /// The URL to GET. A status of 400 or more is down.
    #[arg(long)]
    url: Option<String>,
    /// Up only when the first 1 MiB of the body contains this text.
    #[arg(long, conflicts_with_all = ["no_keyword", "heartbeat"])]
    keyword: Option<String>,
    /// Up only when the first 1 MiB of the body does not contain this text.
    #[arg(long, conflicts_with = "heartbeat")]
    no_keyword: Option<String>,
    /// Expect a ping to /api/heartbeat/<token> every --every.
    #[arg(long)]
    heartbeat: bool,
    /// Default 60s for --url, 1h for --heartbeat.
    #[arg(long, value_parser = duration)]
    every: Option<Duration>,
    /// Default 10s.
    #[arg(long, value_parser = duration, conflicts_with = "heartbeat")]
    timeout: Option<Duration>,
    /// How late a ping may be before the monitor is down. Default 5m.
    #[arg(long, value_parser = duration, conflicts_with = "url")]
    grace: Option<Duration>,
    /// Also take Sentry SDK check-ins for this monitor slug. They ping the
    /// monitor but never change its --every or --grace. Anyone with the
    /// public project DSN can ping a guessable slug; keep it unset for token-only access.
    #[arg(long, conflicts_with = "url")]
    slug: Option<String>,
}

#[derive(Args)]
#[command(group(ArgGroup::new("change").required(true).multiple(true).args([
    "project", "name", "every", "url", "keyword", "no_keyword", "status_only", "timeout", "grace",
    "slug", "no_slug",
])))]
#[command(group(ArgGroup::new("body").args(["keyword", "no_keyword", "status_only"])))]
pub struct EditArgs {
    /// The ID that `davy monitor list` prints.
    id: i64,
    #[arg(long)]
    data: PathBuf,
    /// Moves the monitor, with its checks and incidents, to this project slug.
    #[arg(long)]
    project: Option<String>,
    /// A name for alerts and lists, such as `api health`.
    #[arg(long)]
    name: Option<String>,
    /// How often an HTTP check runs, or a heartbeat expects a ping.
    #[arg(long, value_parser = duration)]
    every: Option<Duration>,
    /// The URL an HTTP check GETs.
    #[arg(long)]
    url: Option<String>,
    /// Up only when the first 1 MiB of the body contains this text.
    #[arg(long)]
    keyword: Option<String>,
    /// Up only when the first 1 MiB of the body does not contain this text.
    #[arg(long)]
    no_keyword: Option<String>,
    /// Up on the status alone, without a keyword.
    #[arg(long)]
    status_only: bool,
    /// How long an HTTP check may take.
    #[arg(long, value_parser = duration)]
    timeout: Option<Duration>,
    /// How late a heartbeat's ping may be before the monitor is down.
    #[arg(long, value_parser = duration)]
    grace: Option<Duration>,
    /// Also take Sentry SDK check-ins for this monitor slug, as add --slug.
    #[arg(long, conflicts_with = "no_slug")]
    slug: Option<String>,
    /// Stop taking check-ins, so only the heartbeat token pings the monitor.
    #[arg(long)]
    no_slug: bool,
    #[arg(long)]
    json: bool,
}

fn duration(s: &str) -> Result<Duration, String> {
    time::duration(s).ok_or_else(|| format!("expected 30s, 15m, 1h or 2d, got {s:?}"))
}

/// Text from ingest with every control character replaced by U+FFFD, so a
/// stored string cannot move the cursor, retitle the terminal or split a line.
fn safe(s: &str) -> Cow<'_, str> {
    text::printable(s, char::REPLACEMENT_CHARACTER)
}

pub fn run(out: &mut impl Write, command: Monitor) -> anyhow::Result<()> {
    match command {
        Monitor::Add(args) => add(out, args)?,
        Monitor::List { data, json } => list(out, &data, json)?,
        Monitor::Edit(args) => edit(out, args)?,
        Monitor::Remove { id, data } => {
            anyhow::ensure!(
                store::remove(&crate::store::open(&data)?, id)?,
                "no monitor {id}"
            );
        }
    }
    Ok(())
}

fn add(out: &mut impl Write, args: Add) -> anyhow::Result<()> {
    let c = crate::store::open(&args.data)?;
    let project = crate::store::project_by_slug(&c, &args.project)?;
    let target = match args.url {
        Some(url) => Target::Http {
            url: CheckUrl::parse(&url).map_err(anyhow::Error::msg)?,
            expect: match (args.keyword, args.no_keyword) {
                (Some(k), _) => Expect::Keyword(k),
                (None, Some(k)) => Expect::NoKeyword(k),
                (None, None) => Expect::Status,
            },
            timeout: args.timeout.unwrap_or(super::DEFAULT_TIMEOUT),
        },
        None => Target::Heartbeat {
            grace: args.grace.unwrap_or(super::DEFAULT_GRACE),
            slug: args.slug.as_deref().map(slug).transpose()?,
        },
    };
    let m = NewMonitor::new(&args.name, target, args.every).map_err(anyhow::Error::msg)?;
    let added = store::add(&c, project, &m, time::now_ms())?;
    writeln!(out, "{}\t{}", added.id, added.target)?;
    Ok(())
}

fn edit(out: &mut impl Write, args: EditArgs) -> anyhow::Result<()> {
    let mut c = crate::store::open(&args.data)?;
    let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let project = args
        .project
        .map(|slug| crate::store::project_by_slug(&tx, &slug))
        .transpose()?;
    let expect = match (args.keyword, args.no_keyword, args.status_only) {
        (Some(k), _, _) => Some(Expect::Keyword(k)),
        (None, Some(k), _) => Some(Expect::NoKeyword(k)),
        (None, None, true) => Some(Expect::Status),
        (None, None, false) => None,
    };
    let slug = match (args.slug, args.no_slug) {
        (Some(s), _) => Some(Some(slug(&s)?)),
        (None, true) => Some(None),
        (None, false) => None,
    };
    let change = Edit {
        project,
        name: args.name,
        every: args.every,
        url: args
            .url
            .map(|u| CheckUrl::parse(&u).map_err(anyhow::Error::msg))
            .transpose()?,
        expect,
        timeout: args.timeout,
        grace: args.grace,
        slug,
    };
    let edited = super::edit::edit(&tx, args.id, &change, time::now_ms())?;
    let row = args
        .json
        .then(|| store::one(&tx, args.id))
        .transpose()?
        .flatten();
    tx.commit()?;
    match row {
        Some(row) => writeln!(out, "{}", serde_json::to_string(&row)?)?,
        None => writeln!(out, "{}\t{}", edited.id, edited.target)?,
    }
    Ok(())
}

fn slug(s: &str) -> anyhow::Result<MonitorSlug> {
    MonitorSlug::parse(s)
        .ok_or_else(|| anyhow::anyhow!("a slug is 1 to 50 characters of A-Z a-z 0-9 _ -"))
}

fn list(out: &mut impl Write, data: &std::path::Path, json: bool) -> anyhow::Result<()> {
    let monitors = store::list(&crate::store::open(data)?)?;
    if json {
        writeln!(out, "{}", serde_json::to_string(&monitors)?)?;
        return Ok(());
    }
    let at = |ms: Option<i64>| ms.map_or_else(|| "-".to_owned(), time::rfc3339_ms);
    for m in monitors {
        writeln!(
            out,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}",
            m.id,
            safe(&m.project),
            safe(&m.name),
            m.state.as_str(),
            safe(&m.target),
            at(m.last_check_at),
            at(m.open_since)
        )?;
    }
    Ok(())
}
