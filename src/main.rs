use clap::{Parser, Subcommand};
use davy::{
    http,
    logs::{
        LogLevel,
        store::{LogHit, LogQuery, search},
    },
    store,
    time::{self, now_ns},
};
use std::{
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
    Serve {
        #[arg(long)]
        data: PathBuf,
        #[arg(long)]
        listen: String,
        #[arg(long)]
        public_url: url::Url,
        #[arg(long, default_value_t = 30)]
        log_retention_days: u32,
    },
    Project {
        #[command(subcommand)]
        command: Project,
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
    Logs {
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
    },
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
fn validate_url(url: &url::Url) -> anyhow::Result<()> {
    anyhow::ensure!(
        matches!(url.scheme(), "http" | "https") && url.host_str().is_some(),
        "public URL must be HTTP(S)"
    );
    Ok(())
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut out = std::io::stdout();
    match Cli::parse().command {
        Command::Serve {
            data,
            listen,
            public_url,
            log_retention_days,
        } => {
            validate_url(&public_url)?;
            anyhow::ensure!(
                log_retention_days >= 1,
                "--log-retention-days must be at least 1"
            );
            http::serve(&data, &listen, log_retention_days).await?;
        }
        Command::Project {
            command:
                Project::Create {
                    slug,
                    data,
                    public_url,
                },
        } => writeln!(out, "{}", create_project(&data, &slug, public_url)?)?,
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
        Command::Logs {
            query,
            data,
            project,
            since,
            until,
            level,
            limit,
            json,
        } => {
            let project = match project {
                Some(slug) => Some(
                    store::project_id(&store::open(&data)?, &slug)?
                        .ok_or_else(|| anyhow::anyhow!("unknown project {slug:?}"))?,
                ),
                None => None,
            };
            let hits = search(
                &data.join("logs"),
                &LogQuery {
                    project,
                    since,
                    until: until.unwrap_or_else(now_ns),
                    min_level: level,
                    text: query,
                    limit,
                },
            )?;
            print_logs(&mut out.lock(), hits, json)?;
        }
    }
    Ok(())
}
/// Creates the project and returns its DSN.
fn create_project(data: &Path, slug: &str, mut public_url: url::Url) -> anyhow::Result<url::Url> {
    validate_url(&public_url)?;
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
            i.status,
            i.count,
            i.title,
            i.culprit.unwrap_or_default()
        )?;
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
            hit.service.as_deref().unwrap_or("-"),
            hit.message
        )?;
        for (k, v) in &hit.attributes {
            match v {
                serde_json::Value::String(s) => write!(out, " {k}={s}")?,
                other => write!(out, " {k}={other}")?,
            }
        }
        writeln!(out)?;
    }
    Ok(())
}
