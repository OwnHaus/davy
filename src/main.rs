use clap::{ArgGroup, CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};
use davy::{
    alert, context, feedback, install, issues, ledger,
    logs::{
        self, LogLevel,
        store::{
            LogCounts, LogHit, LogQuery, PatternQuery, PatternReport,
            cold::{self, ColdNote},
            histogram, search_for, top_patterns,
        },
    },
    monitor::{self, CheckUrl},
    query, retention, store, text,
    time::{self, now_ns},
};
use std::{
    borrow::Cow,
    io::Write,
    path::{Path, PathBuf},
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
        command: monitor::cli::Monitor,
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
    /// Agent tokens for /mcp, each with a level: read, triage or full.
    Token {
        #[command(subcommand)]
        command: davy::tokens::cli::Token,
    },
    /// Discards an issue, so its new events are counted and not stored.
    Issue {
        #[command(subcommand)]
        command: davy::discard::Issue,
    },
    /// Drop rules: lines a project's logs drop before they are stored.
    Drop {
        #[command(subcommand)]
        command: davy::drops::Drop,
    },
    /// Lists issues, the most recently seen first. A discarded issue says
    /// so after its status, and in `discarded_at` with --json.
    Issues {
        #[arg(long)]
        data: PathBuf,
        /// Only the issues of this project slug.
        #[arg(long)]
        project: Option<String>,
        /// Only the issues that `davy issue discard` discarded.
        #[arg(long)]
        discarded: bool,
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
    /// Prints what each project stores, how fast it grows and when the disk
    /// fills. Opens the database read-only, so it runs next to `davy serve`.
    Usage(davy::usage::Args),
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
    /// Saves a test panic in DATA/crashes, as a panic of `davy serve` would
    /// be, and exits with 101. `davy serve` records it in Davy's own project.
    #[command(hide = true)]
    Crash {
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
    /// Only the logs of this pattern: an id `--patterns` prints, or `other`.
    #[arg(long, value_parser = logs::patterns::parse)]
    pattern: Option<u64>,
    /// At most this many logs, the newest ones in the window, or with
    /// --patterns this many patterns, at most 100.
    #[arg(long, default_value_t = 100, conflicts_with = "count")]
    limit: usize,
    /// Prints how many logs match per time bucket instead of the logs.
    #[arg(long)]
    count: bool,
    /// Prints the patterns with the most bytes in the window instead of the
    /// logs, counted in whole UTC days.
    #[arg(long, conflicts_with_all = ["count", "query", "pattern"])]
    patterns: bool,
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
    /// Read or replace server-side scrubbing settings.
    Scrub(davy::scrub::settings::Args),
    /// Sets or clears the secret a Vercel Log Drain signs with, and prints
    /// whether drain requests must be signed. The secret comes on standard
    /// input and is never printed.
    Vercel(davy::vercel::Args),
    /// Shows or sets a project's daily quotas, error events and log bytes
    /// a UTC day, and its spike rates, error events and log records a
    /// second. Past either, ingest answers 429. A running server applies a
    /// change within 5 s. A project can pass its quota by the requests in
    /// flight when it reached it, and a hard kill of the server can lose up
    /// to 5 s of counts.
    Quota(davy::quota::cli::Args),
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
    /// Sets how many days a project keeps its logs and errors, and prints
    /// its own settings as JSON, null where it follows the server's
    /// --log-retention-days or --error-retention-days. With no flag it only
    /// prints them. A running server deletes what is older within a minute,
    /// and an issue goes with its last event.
    Retention {
        /// The project's slug.
        slug: String,
        #[arg(long)]
        data: PathBuf,
        /// Days of logs to keep, 1 to 36500, or `default` for the server's.
        #[arg(long, value_parser = retention::Setting::parse)]
        logs: Option<retention::Setting>,
        /// Days of errors to keep, by when Davy received them: events,
        /// their issues and user feedback. 1 to 36500, or `default`.
        #[arg(long, value_parser = retention::Setting::parse)]
        errors: Option<retention::Setting>,
    },
}
#[derive(Subcommand)]
enum Channel {
    /// Adds a webhook, a Slack incoming webhook or an e-mail address for
    /// --project or --all. Prints the channel's ID, and for a webhook its
    /// signing secret, shown only this once.
    Add(ChannelAdd),
    /// Prints each channel with its unsent, failed and dropped notices, and
    /// the notices of monitors made by check-ins that it dropped.
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
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Every file Davy creates holds events, logs, sessions or keys.
    #[cfg(unix)]
    nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o077));
    let mut out = std::io::stdout();
    let parsed = Cli::from_arg_matches(&cli().get_matches()).unwrap_or_else(|e| e.exit());
    match parsed.command {
        Command::Connect(args) => davy::connect::run(&mut out.lock(), &args)?,
        Command::Project { command } => run_project(&mut out.lock(), command)?,
        Command::Serve(args) => davy::serve::run(args).await?,
        Command::SystemdUnit(args) => install::unit::run(&mut out.lock(), &args)?,
        Command::Setup(args) => install::setup::run(&mut out.lock(), &args)?,
        Command::Doctor(args) => install::doctor::run(&mut out.lock(), &args).await?,
        Command::McpConfig(args) => install::client::run(&mut out.lock(), &args)?,
        Command::Feedback(args) => feedback::run(&mut out.lock(), &args)?,
        Command::Monitor { command } => monitor::cli::run(&mut out.lock(), command)?,
        Command::Channel { command } => run_channel(&mut out.lock(), command)?,
        Command::User { command } => davy::accounts::cli::run(&mut out.lock(), command)?,
        Command::Token { command } => davy::tokens::cli::run(&mut out.lock(), command)?,
        Command::Issue { command } => davy::discard::run(&mut out.lock(), command)?,
        Command::Drop { command } => {
            let stdin = std::io::stdin();
            let mut terminal = std::io::IsTerminal::is_terminal(&stdin).then(|| stdin.lock());
            let answer = terminal.as_mut().map(|t| t as &mut dyn std::io::BufRead);
            davy::drops::run(&mut out.lock(), &mut std::io::stderr(), answer, command)?;
        }
        Command::Issues {
            data,
            project,
            discarded,
            json,
        } => {
            let issues = store::issues(&store::open(&data)?, project.as_deref(), discarded)?;
            print_issues(&mut out.lock(), issues, json)?;
        }
        Command::Event { issue_id, data } => {
            let payload = store::payload(&store::open(&data)?, issue_id)?
                .ok_or_else(|| anyhow::anyhow!("no stored event for issue {issue_id}"))?;
            writeln!(out, "{}", serde_json::to_string_pretty(&payload)?)?;
        }
        Command::Logs(args) => logs(&mut out.lock(), args)?,
        Command::Crash { data } => davy::self_report::crash::test_panic(&data),
        Command::Usage(args) => davy::usage::run(&mut out.lock(), &args)?,
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
            writeln!(out, "{}", text::json(&context, true)?)?;
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
            let ids = issues::IssueIds::one(issue_id);
            let what = ledger::describe::status(ids.ids(), &status);
            let entry = ledger::Entry::of(ledger::Who::Cli, "issue_status", what);
            let c = store::open(&data)?;
            ledger::recorded(&c, entry, |c| issues::set_status(c, &ids, &status))?
                .map_err(anyhow::Error::msg)?;
        }
    }
    Ok(())
}
fn run_project(out: &mut impl Write, command: Project) -> anyhow::Result<()> {
    match command {
        Project::Scrub(args) => davy::scrub::settings::run(out, &args)?,
        Project::Vercel(args) => davy::vercel::run(out, &args, std::io::stdin().lock())?,
        Project::Quota(args) => davy::quota::cli::run(out, &args)?,
        Project::List { data } => davy::connect::list(out, &data)?,
        Project::Create {
            slug,
            data,
            public_url,
        } => {
            let public_url = match public_url {
                Some(url) => url,
                None => install::Settings::load(&data)?.public_url()?,
            };
            writeln!(out, "{}", create_project(&data, &slug, &public_url)?)?;
        }
        Project::Retention {
            slug,
            data,
            logs,
            errors,
        } => {
            let c = store::open(&data)?;
            let change = retention::Change {
                project: slug,
                logs_days: logs,
                errors_days: errors,
            };
            let stored = if logs.is_none() && errors.is_none() {
                retention::stored(&c, &change.project)?
                    .ok_or_else(|| retention::unknown(&change.project))
            } else {
                // A server's log writer may be deleting by the settings
                // this replaces, so the change waits for it.
                let lock = retention::Lock::default()
                    .with_dir(&data)
                    .waiting(retention::CLI_WAIT);
                let _held = lock.hold_all()?;
                let what = ledger::describe::retention(&change);
                let entry = ledger::Entry::of(ledger::Who::Cli, "set_retention", what);
                ledger::recorded(&c, entry, |c| {
                    retention::set(c, &change, retention::By::Person)
                })?
            };
            let stored = stored.map_err(anyhow::Error::msg)?;
            let shown = serde_json::json!({
                "project": change.project,
                "logs_days": stored.logs_days,
                "errors_days": stored.errors_days,
            });
            writeln!(out, "{shown}")?;
        }
    }
    Ok(())
}
fn logs(out: &mut impl Write, args: Logs) -> anyhow::Result<()> {
    let project = match args.project {
        Some(slug) => Some(store::project_by_slug(&store::open(&args.data)?, &slug)?),
        None => None,
    };
    let until = args.until.unwrap_or_else(now_ns);
    if args.patterns {
        let q = PatternQuery {
            project,
            since: args.since,
            until,
            min_level: args.level,
            limit: args.limit,
        };
        return print_patterns(out, &top_patterns(&logs::dir(&args.data), &q)?, args.json);
    }
    let q = LogQuery {
        project,
        since: args.since,
        until,
        min_level: args.level,
        text: args.query,
        pattern: args.pattern,
        limit: args.limit,
        skip: 0,
    };
    if !args.count {
        let found = search_for(&logs::dir(&args.data), &q, cold::BUDGET)?;
        print_logs(out, found.hits, args.json)?;
        return cold_line(found.cold.as_ref().map(ColdNote::from).as_ref());
    }
    let counts = LogCounts::from(histogram(&logs::dir(&args.data), &q)?);
    if args.json {
        writeln!(out, "{}", serde_json::to_string(&counts)?)?;
        return Ok(());
    }
    writeln!(out, "{} logs, {} buckets", counts.total, counts.step)?;
    for (start, n) in &counts.buckets {
        writeln!(out, "{start}\t{n}")?;
    }
    cold_line(counts.cold.as_ref())
}
/// What a search read of cold days, on stderr so that stdout stays rows.
fn cold_line(note: Option<&ColdNote>) -> anyhow::Result<()> {
    if let Some(note) = note {
        writeln!(std::io::stderr(), "{}", note.line())?;
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
                    "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    ch.id,
                    ch.project.as_deref().map_or(Cow::Borrowed("*"), safe),
                    ch.kind.as_str(),
                    safe(&ch.to),
                    ch.unsent,
                    ch.failed,
                    ch.dropped,
                    ch.sdk_dropped
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
        writeln!(out, "{}", text::json(&issues, false)?)?;
        return Ok(());
    }
    for i in issues {
        let mark = if i.discarded_at.is_some() {
            " (discarded)"
        } else {
            ""
        };
        writeln!(
            out,
            "{}\t{}{mark}\t{}\t{}\t{}",
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
/// The report, a line per pattern with the most bytes first, after a summary.
fn print_patterns(out: &mut impl Write, r: &PatternReport, json: bool) -> anyhow::Result<()> {
    if json {
        writeln!(out, "{}", serde_json::to_string(r)?)?;
        return Ok(());
    }
    writeln!(
        out,
        "{} to {}: {} logs, {}, {} patterns",
        r.from, r.to, r.records, r.bytes, r.patterns
    )?;
    if !r.unread.is_empty() {
        let days = r.unread.join(", ");
        writeln!(out, "left out, counters unreadable: {days}")?;
    }
    if !r.restarted.is_empty() {
        let days = r.restarted.join(", ");
        writeln!(out, "counted in part, counters restarted: {days}")?;
    }
    if r.truncated {
        writeln!(
            out,
            "left out, past the bound of a report: the days before {}",
            r.from
        )?;
    }
    for p in &r.top {
        let template = safe(&p.template);
        writeln!(
            out,
            "{}\t{}\t{}\t{}\t{template}",
            p.bytes, p.share, p.records, p.id
        )?;
    }
    Ok(())
}

/// Text from ingest with every control character replaced by U+FFFD, so a
/// stored string cannot move the cursor, retitle the terminal or split a line.
fn safe(s: &str) -> Cow<'_, str> {
    text::printable(s, char::REPLACEMENT_CHARACTER)
}
#[cfg(test)]
#[path = "main_tests.rs"]
mod tests;
