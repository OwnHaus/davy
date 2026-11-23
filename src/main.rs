use clap::{ArgGroup, CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};
use davy::{
    alert, context, feedback, install, issues,
    logs::{
        self, LogLevel,
        store::{LogCounts, LogHit, LogQuery, histogram, search},
    },
    monitor::{self, CheckUrl, Expect, NewMonitor, Target},
    query, store, text,
    time::{self, now_ns},
};
use std::{
    borrow::Cow,
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Prepares a data directory, an admin token and a service file, and
    /// prints the next commands as JSON. Safe to run again.
    Setup(install::setup::Args),
    /// Creates or reuses a project and prints SDK connection instructions as JSON.
    Connect(davy::connect::Args),
    /// Checks a running install. Each check that is not ok prints its fix. The
    /// exit code is 1 when a check fails.
    Doctor(install::doctor::Args),
    /// Prints the MCP config for Claude Code, Codex or Cursor. The config
    /// reads the token from DAVY_TOKEN, so it can be committed.
    McpConfig(install::client::Args),
    /// Lists what agents reported about Davy's tools and docs with the MCP
    /// tool submit_feedback, oldest first.
    Feedback(feedback::Args),
    /// Prints the systemd unit for the data directory. Run it as root or the
    /// operator, never as the service user, and write the output to
    /// /etc/systemd/system/davy.service.
    SystemdUnit(install::unit::Args),
    /// Runs the ingest server. With an admin token in DAVY_TOKEN, a file named
    /// by DAVY_TOKEN_FILE or DATA/token it also serves MCP at /mcp and takes
    /// source map uploads.
    Serve(davy::serve::Args),
    /// The apps that send errors and logs, each with its own DSN.
    Project {
        #[command(subcommand)]
        command: Project,
    },
    /// Uptime checks of a URL and heartbeats from cron jobs.
    Monitor {
        #[command(subcommand)]
        command: Monitor,
    },
    /// Where alerts about issues and monitors go.
    Channel {
        #[command(subcommand)]
        command: Channel,
    },
    /// The people who may sign in to the web UI.
    User {
        #[command(subcommand)]
        command: davy::accounts::cli::User,
    },
    /// Lists issues, the most recently seen first.
    Issues {
        #[arg(long)]
        data: PathBuf,
        /// Only the issues of this project slug.
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Prints the newest stored event of an issue as the SDK sent it.
    Event {
        /// The ID that `davy issues` prints.
        issue_id: i64,
        #[arg(long)]
        data: PathBuf,
    },
    /// Search logs, oldest first. QUERY uses Tantivy syntax over the message and
    /// attributes, e.g. `timeout` or `attributes.provider:acme`.
    Logs(Logs),
    /// Runs one read-only SQL query against the error store. `davy sql schema`
    /// prints the tables and notes on units.
    Sql {
        /// One SELECT statement, or `schema`.
        query: String,
        #[arg(long)]
        data: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Prints what an agent needs to fix an issue as JSON: the issue, its newest
    /// event, recent events, releases and the logs around it.
    Context {
        /// The ID that `davy issues` prints.
        issue_id: i64,
        #[arg(long)]
        data: PathBuf,
    },
    /// Marks an issue fixed. Without --release it stays fixed until an event
    /// arrives from a release first seen after now.
    Resolve {
        /// The ID that `davy issues` prints.
        issue_id: i64,
        #[arg(long)]
        data: PathBuf,
        /// The release the fix ships in. An event from it or a newer release
        /// reopens the issue.
        #[arg(long)]
        release: Option<String>,
    },
}
#[derive(clap::Args)]
struct Logs {
    /// Without it every log in the window matches.
    query: Option<String>,
    #[arg(long)]
    data: PathBuf,
    /// Only the logs of this project slug.
    #[arg(long)]
    project: Option<String>,
    /// A duration back from now (30m, 1h, 2d) or an RFC 3339 timestamp.
    #[arg(long, default_value = "24h", value_parser = instant)]
    since: i64,
    /// Like --since. Defaults to now.
    #[arg(long, value_parser = instant)]
    until: Option<i64>,
    /// Minimum level: trace, debug, info, warn, error, fatal.
    #[arg(long, value_parser = level)]
    level: Option<LogLevel>,
    /// At most this many logs, the newest ones in the window.
    #[arg(long, default_value_t = 100, conflicts_with = "count")]
    limit: usize,
    /// Prints how many logs match per time bucket instead of the logs.
    #[arg(long)]
    count: bool,
    #[arg(long)]
    json: bool,
}
fn instant(s: &str) -> Result<i64, String> {
    time::instant(s, now_ns())
}
fn level(s: &str) -> Result<LogLevel, String> {
    LogLevel::named(s)
}
#[derive(Subcommand)]
enum Project {
    /// Prints project IDs and slugs as JSON.
    List {
        #[arg(long)]
        data: PathBuf,
    },
    /// Creates a project and prints its DSN. For an existing slug it prints
    /// the DSN it already has.
    Create {
        /// A short name such as `web` or `billing-api`.
        slug: String,
        #[arg(long)]
        data: PathBuf,
        /// Defaults to the public URL given to `davy setup` for this directory.
        #[arg(long)]
        public_url: Option<url::Url>,
    },
}
#[derive(Subcommand)]
enum Monitor {
    /// Adds an HTTP check with --url, or a heartbeat with --heartbeat. Prints
    /// the monitor's ID and its URL or ping path.
    Add(MonitorAdd),
    /// Prints each monitor with its state, last check and open incident.
    List {
        #[arg(long)]
        data: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Deletes a monitor with its checks and incidents.
    Remove {
        /// The ID that `davy monitor list` prints.
        id: i64,
        #[arg(long)]
        data: PathBuf,
    },
}
#[derive(clap::Args)]
#[command(group(ArgGroup::new("target").required(true).args(["url", "heartbeat"])))]
struct MonitorAdd {
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
}
#[derive(Subcommand)]
enum Channel {
    /// Adds a webhook, a Slack incoming webhook or an e-mail address for
    /// --project or --all. Prints the channel's ID, and for a webhook its
    /// signing secret, shown only this once.
    Add(ChannelAdd),
    /// Prints each channel with its unsent, failed and dropped notices.
    List {
        #[arg(long)]
        data: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Deletes a channel with its notices.
    Remove {
        /// The ID that `davy channel list` prints.
        id: i64,
        #[arg(long)]
        data: PathBuf,
    },
    /// Queues a test notice for one channel. A running server sends it.
    Test {
        /// The ID that `davy channel list` prints.
        id: i64,
        #[arg(long)]
        data: PathBuf,
    },
}
#[derive(clap::Args)]
#[command(group(ArgGroup::new("scope").required(true).args(["project", "all"])))]
struct ChannelAdd {
    /// How alerts are sent.
    kind: ChannelKind,
    /// The URL of a webhook or Slack channel, or the address of an e-mail one.
    to: String,
    /// Alerts for this project slug only.
    #[arg(long)]
    project: Option<String>,
    /// Alerts for every project, including ones created later.
    #[arg(long)]
    all: bool,
    /// Also sends what agents report with the MCP tool submit_feedback.
    #[arg(long)]
    feedback: bool,
    #[arg(long)]
    data: PathBuf,
}
#[derive(Clone, Copy, ValueEnum)]
enum ChannelKind {
    Webhook,
    Slack,
    Email,
}
/// The command line, with help for the flags most commands share.
fn cli() -> clap::Command {
    shared_help(Cli::command())
}
fn shared_help(c: clap::Command) -> clap::Command {
    c.mut_args(|a| match (a.get_id().as_str(), a.get_help()) {
        ("data", None) => a.help("The data directory, such as /var/lib/davy"),
        ("json", None) => a.help("Prints JSON instead of text"),
        _ => a,
    })
    .mut_subcommands(shared_help)
}
fn duration(s: &str) -> Result<Duration, String> {
    time::duration(s).ok_or_else(|| format!("expected 30s, 15m, 1h or 2d, got {s:?}"))
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Every file Davy creates holds events, logs, sessions or keys.
    #[cfg(unix)]
    nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o077));
    let mut out = std::io::stdout();
    let parsed = Cli::from_arg_matches(&cli().get_matches()).unwrap_or_else(|e| e.exit());
    match parsed.command {
        Command::Connect(args) => davy::connect::run(&mut out.lock(), &args)?,
        Command::Project {
            command: Project::List { data },
        } => davy::connect::list(&mut out.lock(), &data)?,
        Command::Serve(args) => davy::serve::run(args).await?,
        Command::SystemdUnit(args) => install::unit::run(&mut out.lock(), &args)?,
        Command::Setup(args) => install::setup::run(&mut out.lock(), &args)?,
        Command::Doctor(args) => install::doctor::run(&mut out.lock(), &args).await?,
        Command::McpConfig(args) => install::client::run(&mut out.lock(), &args)?,
        Command::Feedback(args) => feedback::run(&mut out.lock(), &args)?,
        Command::Project {
            command:
                Project::Create {
                    slug,
                    data,
                    public_url,
                },
        } => {
            let public_url = match public_url {
                Some(url) => url,
                None => install::Settings::load(&data)?.public_url()?,
            };
            writeln!(out, "{}", create_project(&data, &slug, &public_url)?)?;
        }
        Command::Monitor { command } => run_monitor(&mut out.lock(), command)?,
        Command::Channel { command } => run_channel(&mut out.lock(), command)?,
        Command::User { command } => davy::accounts::cli::run(&mut out.lock(), command)?,
        Command::Issues {
            data,
            project,
            json,
        } => {
            let issues = store::issues(&store::open(&data)?, project.as_deref())?;
            print_issues(&mut out.lock(), issues, json)?;
        }
        Command::Event { issue_id, data } => {
            let payload = store::payload(&store::open(&data)?, issue_id)?
                .ok_or_else(|| anyhow::anyhow!("no stored event for issue {issue_id}"))?;
            writeln!(out, "{}", serde_json::to_string_pretty(&payload)?)?;
        }
        Command::Logs(args) => logs(&mut out.lock(), args)?,
        Command::Sql { query, data, json } => {
            let c = query::open(&data)?;
            if query.trim() == "schema" {
                writeln!(out, "{}", query::schema(&c)?)?;
            } else {
                print_rows(&mut out.lock(), &query::run(&c, &query)?, json)?;
            }
        }
        Command::Context { issue_id, data } => {
            let context = context::issue_context(&store::open(&data)?, &data, issue_id)?
                .ok_or_else(|| anyhow::anyhow!("no issue {issue_id}"))?;
            writeln!(out, "{}", serde_json::to_string_pretty(&context)?)?;
        }
        Command::Resolve {
            issue_id,
            data,
            release,
        } => {
            let status = store::IssueStatus::Resolved {
                in_release: release,
                at: time::now_ms(),
            };
            issues::set_status(
                &store::open(&data)?,
                &issues::IssueIds::one(issue_id),
                &status,
            )?
            .map_err(anyhow::Error::msg)?;
        }
    }
    Ok(())
}
fn logs(out: &mut impl Write, args: Logs) -> anyhow::Result<()> {
    let project = match args.project {
        Some(slug) => Some(store::project_by_slug(&store::open(&args.data)?, &slug)?),
        None => None,
    };
    let q = LogQuery {
        project,
        since: args.since,
        until: args.until.unwrap_or_else(now_ns),
        min_level: args.level,
        text: args.query,
        limit: args.limit,
        skip: 0,
    };
    if !args.count {
        return print_logs(out, search(&logs::dir(&args.data), &q)?, args.json);
    }
    let counts = LogCounts::from(histogram(&logs::dir(&args.data), &q)?);
    if args.json {
        writeln!(out, "{}", serde_json::to_string(&counts)?)?;
    } else {
        writeln!(out, "{} logs, {} buckets", counts.total, counts.step)?;
        for (start, n) in counts.buckets {
            writeln!(out, "{start}\t{n}")?;
        }
    }
    Ok(())
}
fn run_monitor(out: &mut impl Write, command: Monitor) -> anyhow::Result<()> {
    match command {
        Monitor::Add(args) => {
            let c = store::open(&args.data)?;
            let project = store::project_by_slug(&c, &args.project)?;
            let target = match args.url {
                Some(url) => Target::Http {
                    url: CheckUrl::parse(&url).map_err(anyhow::Error::msg)?,
                    expect: match (args.keyword, args.no_keyword) {
                        (Some(k), _) => Expect::Keyword(k),
                        (None, Some(k)) => Expect::NoKeyword(k),
                        (None, None) => Expect::Status,
                    },
                    timeout: args.timeout.unwrap_or(monitor::DEFAULT_TIMEOUT),
                },
                None => Target::Heartbeat {
                    grace: args.grace.unwrap_or(monitor::DEFAULT_GRACE),
                },
            };
            let m = NewMonitor::new(&args.name, target, args.every).map_err(anyhow::Error::msg)?;
            let added = monitor::store::add(&c, project, &m, time::now_ms())?;
            writeln!(out, "{}\t{}", added.id, added.target)?;
        }
        Monitor::List { data, json } => {
            let monitors = monitor::store::list(&store::open(&data)?)?;
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
        }
        Monitor::Remove { id, data } => {
            anyhow::ensure!(
                monitor::store::remove(&store::open(&data)?, id)?,
                "no monitor {id}"
            );
        }
    }
    Ok(())
}
fn run_channel(out: &mut impl Write, command: Channel) -> anyhow::Result<()> {
    match command {
        Channel::Add(args) => {
            let c = store::open(&args.data)?;
            let project = match &args.project {
                Some(slug) => Some(store::project_by_slug(&c, slug)?),
                None => None,
            };
            let to = match args.kind {
                ChannelKind::Webhook => CheckUrl::parse(&args.to).map(alert::Destination::Webhook),
                ChannelKind::Slack => CheckUrl::parse(&args.to).map(alert::Destination::Slack),
                ChannelKind::Email => {
                    alert::EmailAddress::parse(&args.to).map(alert::Destination::Email)
                }
            }
            .map_err(anyhow::Error::msg)?;
            let added = alert::store::add(&c, project, &to, args.feedback, time::now_ms())?;
            match added.secret {
                Some(secret) => writeln!(out, "{}\t{}", added.id, secret.as_str())?,
                None => writeln!(out, "{}", added.id)?,
            }
        }
        Channel::List { data, json } => {
            let channels = alert::store::list(&store::open(&data)?)?;
            if json {
                writeln!(out, "{}", serde_json::to_string(&channels)?)?;
                return Ok(());
            }
            for ch in channels {
                writeln!(
                    out,
                    "{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    ch.id,
                    ch.project.as_deref().map_or(Cow::Borrowed("*"), safe),
                    ch.kind.as_str(),
                    safe(&ch.to),
                    ch.unsent,
                    ch.failed,
                    ch.dropped
                )?;
            }
        }
        Channel::Remove { id, data } => {
            anyhow::ensure!(
                alert::store::remove(&store::open(&data)?, id)?,
                "no channel {id}"
            );
        }
        Channel::Test { id, data } => {
            let mut c = store::open(&data)?;
            let tx = c.transaction()?;
            anyhow::ensure!(
                alert::store::test(&tx, id, time::now_ms())?,
                "no channel {id}"
            );
            tx.commit()?;
        }
    }
    Ok(())
}
/// Creates the project, or finds the one with this slug, and returns its DSN.
fn create_project(data: &Path, slug: &str, public_url: &url::Url) -> anyhow::Result<url::Url> {
    install::check_public_url(public_url)?;
    anyhow::ensure!(!slug.trim().is_empty(), "slug must not be empty");
    let (id, key) = store::create_project(&store::open(data)?, slug)?;
    install::dsn(public_url, &key, id)
}
fn print_issues(out: &mut impl Write, issues: Vec<store::Issue>, json: bool) -> anyhow::Result<()> {
    if json {
        writeln!(out, "{}", serde_json::to_string(&issues)?)?;
        return Ok(());
    }
    for i in issues {
        writeln!(
            out,
            "{}\t{}\t{}\t{}\t{}",
            i.id,
            safe(&i.status),
            i.count,
            safe(&i.title),
            safe(i.culprit.as_deref().unwrap_or_default())
        )?;
    }
    Ok(())
}
fn print_rows(out: &mut impl Write, rows: &query::Rows, json: bool) -> anyhow::Result<()> {
    if json {
        writeln!(out, "{}", serde_json::to_string(rows)?)?;
        return Ok(());
    }
    let columns: Vec<_> = rows.columns.iter().map(|c| safe(c)).collect();
    writeln!(out, "{}", columns.join("\t"))?;
    for row in &rows.rows {
        let cells: Vec<String> = row.iter().map(cell).collect();
        writeln!(out, "{}", cells.join("\t"))?;
    }
    if rows.truncated {
        writeln!(std::io::stderr(), "stopped at {} rows", rows.rows.len())?;
    }
    Ok(())
}
/// Prints hits one per line.
fn print_logs(out: &mut impl Write, hits: Vec<LogHit>, json: bool) -> anyhow::Result<()> {
    for hit in hits {
        if json {
            writeln!(out, "{}", serde_json::to_string(&hit)?)?;
            continue;
        }
        write!(
            out,
            "{} {:<5} {} {}",
            hit.timestamp,
            hit.level.as_str(),
            safe(hit.service.as_deref().unwrap_or("-")),
            safe(&hit.message)
        )?;
        for (k, v) in &hit.attributes {
            write!(out, " {}={}", safe(k), cell(v))?;
        }
        writeln!(out)?;
    }
    Ok(())
}
/// A string as its text and any other value as JSON, made [`safe`].
fn cell(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => safe(s).into_owned(),
        other => safe(&other.to_string()).into_owned(),
    }
}
/// Text from ingest with every control character replaced by U+FFFD, so a
/// stored string cannot move the cursor, retitle the terminal or split a line.
fn safe(s: &str) -> Cow<'_, str> {
    text::printable(s, char::REPLACEMENT_CHARACTER)
}
#[cfg(test)]
#[path = "main_tests.rs"]
mod tests;
