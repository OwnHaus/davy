use clap::{ArgGroup, Parser, Subcommand, ValueEnum};
use davy::{
    alert, context, http, install,
    logs::{
        LogLevel,
        store::{LogHit, LogQuery, search},
    },
    monitor::{self, CheckUrl, Expect, NewMonitor, Target},
    query, secret, store,
    time::{self, now_ns},
    token::Token,
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
    /// Runs the ingest server. With an admin token in DAVY_TOKEN or a file named
    /// by DAVY_TOKEN_FILE it also serves MCP at /mcp and takes source map uploads.
    Serve(Serve),
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
    Issues {
        #[arg(long)]
        data: PathBuf,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        json: bool,
    },
    Event {
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
        query: String,
        #[arg(long)]
        data: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Prints what an agent needs to fix an issue as JSON: the issue, its newest
    /// event, recent events, releases and the logs around it.
    Context {
        issue_id: i64,
        #[arg(long)]
        data: PathBuf,
    },
    /// Marks an issue fixed. Without --release it stays fixed until an event
    /// arrives from a release first seen after now.
    Resolve {
        issue_id: i64,
        #[arg(long)]
        data: PathBuf,
        #[arg(long)]
        release: Option<String>,
    },
}
#[derive(clap::Args)]
struct Serve {
    #[arg(long)]
    data: PathBuf,
    #[arg(long)]
    listen: String,
    #[arg(long)]
    public_url: url::Url,
    #[arg(long, default_value_t = 30)]
    log_retention_days: u32,
    /// The sender of alert e-mail, required when DAVY_SMTP_URL or DAVY_SMTP_URL_FILE is set.
    #[arg(long)]
    mail_from: Option<String>,
}
#[derive(clap::Args)]
struct Logs {
    query: Option<String>,
    #[arg(long)]
    data: PathBuf,
    #[arg(long)]
    project: Option<String>,
    /// A duration back from now (30m, 1h, 2d) or an RFC 3339 timestamp.
    #[arg(long, default_value = "24h", value_parser = instant)]
    since: i64,
    #[arg(long, value_parser = instant)]
    until: Option<i64>,
    /// Minimum level: trace, debug, info, warn, error, fatal.
    #[arg(long, value_parser = level)]
    level: Option<LogLevel>,
    #[arg(long, default_value_t = 100)]
    limit: usize,
    #[arg(long)]
    json: bool,
}
fn instant(s: &str) -> Result<i64, String> {
    time::instant(s, now_ns())
}
fn level(s: &str) -> Result<LogLevel, String> {
    LogLevel::parse(s).ok_or_else(|| format!("unknown level {s:?}"))
}
#[derive(Subcommand)]
enum Project {
    Create {
        slug: String,
        #[arg(long)]
        data: PathBuf,
        #[arg(long)]
        public_url: url::Url,
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
        id: i64,
        #[arg(long)]
        data: PathBuf,
    },
}
#[derive(clap::Args)]
#[command(group(ArgGroup::new("target").required(true).args(["url", "heartbeat"])))]
struct MonitorAdd {
    project: String,
    name: String,
    #[arg(long)]
    data: PathBuf,
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
        id: i64,
        #[arg(long)]
        data: PathBuf,
    },
    /// Queues a test notice for one channel. A running server sends it.
    Test {
        id: i64,
        #[arg(long)]
        data: PathBuf,
    },
}
#[derive(clap::Args)]
#[command(group(ArgGroup::new("scope").required(true).args(["project", "all"])))]
struct ChannelAdd {
    kind: ChannelKind,
    /// The URL of a webhook or Slack channel, or the address of an e-mail one.
    to: String,
    #[arg(long)]
    project: Option<String>,
    /// Alerts for every project, including ones created later.
    #[arg(long)]
    all: bool,
    #[arg(long)]
    data: PathBuf,
}
#[derive(Clone, Copy, ValueEnum)]
enum ChannelKind {
    Webhook,
    Slack,
    Email,
}
fn duration(s: &str) -> Result<Duration, String> {
    time::duration(s).ok_or_else(|| format!("expected 30s, 15m, 1h or 2d, got {s:?}"))
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut out = std::io::stdout();
    match Cli::parse().command {
        Command::Serve(args) => serve(args).await?,
        Command::Setup(args) => install::setup::run(&mut out.lock(), &args)?,
        Command::Project {
            command:
                Project::Create {
                    slug,
                    data,
                    public_url,
                },
        } => writeln!(out, "{}", create_project(&data, &slug, public_url)?)?,
        Command::Monitor { command } => run_monitor(&mut out.lock(), command)?,
        Command::Channel { command } => run_channel(&mut out.lock(), command)?,
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
            let found = store::resolve(
                &store::open(&data)?,
                issue_id,
                release.as_deref(),
                time::now_ms(),
            )?;
            anyhow::ensure!(found, "no issue {issue_id}");
        }
    }
    Ok(())
}
async fn serve(args: Serve) -> anyhow::Result<()> {
    install::check_public_url(&args.public_url)?;
    let env = |key: &str| std::env::var(key).ok();
    let token = secret::admin_token(env)?;
    let smtp = match (secret::read("DAVY_SMTP_URL", env)?, &args.mail_from) {
        (Some(url), Some(from)) => Some(alert::mail::Smtp::new(&url, from)?),
        (Some(_), None) => anyhow::bail!("DAVY_SMTP_URL needs --mail-from"),
        (None, Some(_)) => anyhow::bail!("--mail-from needs DAVY_SMTP_URL"),
        (None, None) => None,
    };
    http::serve(
        &args.data,
        &args.listen,
        &args.public_url,
        args.log_retention_days,
        token.as_deref().map(Token::new),
        smtp,
    )
    .await
}
fn logs(out: &mut impl Write, args: Logs) -> anyhow::Result<()> {
    let project = match args.project {
        Some(slug) => Some(
            store::project_id(&store::open(&args.data)?, &slug)?
                .ok_or_else(|| anyhow::anyhow!("unknown project {slug:?}"))?,
        ),
        None => None,
    };
    let hits = search(
        &args.data.join("logs"),
        &LogQuery {
            project,
            since: args.since,
            until: args.until.unwrap_or_else(now_ns),
            min_level: args.level,
            text: args.query,
            limit: args.limit,
        },
    )?;
    print_logs(out, hits, args.json)
}
fn run_monitor(out: &mut impl Write, command: Monitor) -> anyhow::Result<()> {
    match command {
        Monitor::Add(args) => {
            let c = store::open(&args.data)?;
            let project = store::project_id(&c, &args.project)?
                .ok_or_else(|| anyhow::anyhow!("unknown project {:?}", args.project))?;
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
                Some(slug) => Some(
                    store::project_id(&c, slug)?
                        .ok_or_else(|| anyhow::anyhow!("unknown project {slug:?}"))?,
                ),
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
            let added = alert::store::add(&c, project, &to, time::now_ms())?;
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
/// Creates the project and returns its DSN.
fn create_project(data: &Path, slug: &str, mut public_url: url::Url) -> anyhow::Result<url::Url> {
    install::check_public_url(&public_url)?;
    anyhow::ensure!(!slug.trim().is_empty(), "slug must not be empty");
    let (id, key) = store::create_project(&store::open(data)?, slug)?;
    let cannot_carry_key = |()| anyhow::anyhow!("public URL cannot carry a key");
    public_url.set_username(&key).map_err(cannot_carry_key)?;
    public_url.set_password(None).map_err(cannot_carry_key)?;
    public_url.set_path(&format!("/{id}"));
    public_url.set_query(None);
    public_url.set_fragment(None);
    Ok(public_url)
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
        let cells: Vec<String> = row
            .iter()
            .map(|v| match v {
                serde_json::Value::String(s) => safe(s).into_owned(),
                other => safe(&other.to_string()).into_owned(),
            })
            .collect();
        writeln!(out, "{}", cells.join("\t"))?;
    }
    if rows.truncated {
        writeln!(std::io::stderr(), "stopped at {} rows", rows.rows.len())?;
    }
    Ok(())
}
/// Prints hits oldest first, one per line.
fn print_logs(out: &mut impl Write, mut hits: Vec<LogHit>, json: bool) -> anyhow::Result<()> {
    hits.reverse();
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
            match v {
                serde_json::Value::String(s) => write!(out, " {}={}", safe(k), safe(s))?,
                other => write!(out, " {}={}", safe(k), safe(&other.to_string()))?,
            }
        }
        writeln!(out)?;
    }
    Ok(())
}
/// Text from ingest with every control character replaced by U+FFFD, so a
/// stored string cannot move the cursor, retitle the terminal or split a line.
fn safe(s: &str) -> Cow<'_, str> {
    if s.chars().any(char::is_control) {
        Cow::Owned(
            s.chars()
                .map(|c| if c.is_control() { '\u{fffd}' } else { c })
                .collect(),
        )
    } else {
        Cow::Borrowed(s)
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_admin_token_is_not_a_command_line_flag() {
        let serve = |extra: &[&str]| {
            let base = ["davy", "serve", "--data", "d", "--listen", "127.0.0.1:0"];
            let url = ["--public-url", "http://127.0.0.1/"];
            Cli::try_parse_from(base.iter().chain(&url).chain(extra)).is_ok()
        };
        assert!(serve(&[]));
        assert!(!serve(&["--token", "0123456789abcdef0123456789abcdef"]));
    }

    #[test]
    fn monitor_add_takes_the_flags_of_one_kind() {
        let add = |extra: &[&str]| {
            let base = ["davy", "monitor", "add", "shop", "API", "--data", "d"];
            Cli::try_parse_from(base.iter().chain(extra)).is_ok()
        };
        assert!(add(&["--url", "https://shop.example/", "--timeout", "5s"]));
        assert!(add(&["--heartbeat", "--grace", "1m"]));
        for bad in [
            &[][..],
            &["--url", "https://shop.example/", "--heartbeat"],
            &["--heartbeat", "--timeout", "5s"],
            &["--heartbeat", "--keyword", "ok"],
            &["--url", "https://shop.example/", "--grace", "1m"],
            &[
                "--url",
                "https://shop.example/",
                "--keyword",
                "a",
                "--no-keyword",
                "b",
            ],
        ] {
            assert!(!add(bad), "{bad:?}");
        }
    }

    #[test]
    fn channel_add_takes_a_project_or_all() {
        let add = |extra: &[&str]| {
            let base = ["davy", "channel", "add", "--data", "d"];
            Cli::try_parse_from(base.iter().chain(extra)).is_ok()
        };
        assert!(add(&[
            "webhook",
            "https://hooks.example/",
            "--project",
            "shop"
        ]));
        assert!(add(&["slack", "https://hooks.slack.com/", "--all"]));
        assert!(add(&["email", "ops@example.com", "--all"]));
        for bad in [
            &["webhook", "https://hooks.example/"][..],
            &[
                "webhook",
                "https://hooks.example/",
                "--project",
                "shop",
                "--all",
            ],
            &["sms", "+420123456789", "--all"],
        ] {
            assert!(!add(bad), "{bad:?}");
        }
    }

    #[test]
    fn sql_text_output_separates_columns_with_tabs() {
        let mut out = Vec::new();
        let rows = query::Rows {
            columns: vec!["id".into(), "na\u{1b}me".into()],
            rows: vec![vec![serde_json::json!(1), serde_json::json!("API\t")]],
            truncated: false,
        };
        print_rows(&mut out, &rows, false).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "id\tna\u{fffd}me\n1\tAPI\u{fffd}\n"
        );
    }

    #[test]
    fn text_output_replaces_control_characters() {
        let mut out = Vec::new();
        let issue = store::Issue {
            id: 1,
            status: "unresolved".into(),
            count: 2,
            title: "Error: \u{1b}]52;c;cHduZWQ=\u{7}".into(),
            culprit: Some("a\tb\u{9b}2J".into()),
        };
        print_issues(&mut out, vec![issue], false).unwrap();
        let hit = LogHit {
            timestamp: "2026-11-20T10:00:00Z".into(),
            timestamp_ns: 0,
            project_id: 1,
            source: davy::logs::LogSource::Logtail,
            level: LogLevel::Info,
            service: Some("api\r".into()),
            message: "line\n\u{1b}[2Jgone".into(),
            trace_id: None,
            span_id: None,
            attributes: serde_json::json!({"k\u{1b}": "v\u{7}"})
                .as_object()
                .unwrap()
                .clone(),
        };
        print_logs(&mut out, vec![hit], false).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "1\tunresolved\t2\tError: \u{fffd}]52;c;cHduZWQ=\u{fffd}\ta\u{fffd}b\u{fffd}2J\n\
             2026-11-20T10:00:00Z info  api\u{fffd} line\u{fffd}\u{fffd}[2Jgone k\u{fffd}=v\u{fffd}\n"
        );
    }
}
