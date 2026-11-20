use clap::{Parser, Subcommand};
use davy::{
    http,
    logs::{
        LogLevel, now_ns,
        store::{LogQuery, search},
    },
    store,
};
use std::path::PathBuf;
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
    if let Ok(t) = chrono::DateTime::parse_from_rfc3339(s) {
        return t
            .timestamp_nanos_opt()
            .ok_or_else(|| "timestamp out of range".into());
    }
    let unit = match s.chars().last() {
        Some('s') => 1,
        Some('m') => 60,
        Some('h') => 3600,
        Some('d') => 86_400,
        _ => return Err("expected 30m, 1h, 2d or an RFC 3339 timestamp".into()),
    };
    let n: i64 = s[..s.len() - 1]
        .parse()
        .map_err(|_| "expected 30m, 1h, 2d or an RFC 3339 timestamp".to_owned())?;
    Ok(now_ns() - n * unit * 1_000_000_000)
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
                    mut public_url,
                },
        } => {
            validate_url(&public_url)?;
            anyhow::ensure!(!slug.trim().is_empty(), "slug must not be empty");
            let (id, key) = store::create_project(&store::open(&data)?, &slug)?;
            public_url
                .set_username(&key)
                .map_err(|_| anyhow::anyhow!("invalid URL"))?;
            public_url
                .set_password(None)
                .map_err(|_| anyhow::anyhow!("invalid URL"))?;
            public_url.set_path(&format!("/{id}"));
            public_url.set_query(None);
            public_url.set_fragment(None);
            println!("{public_url}");
        }
        Command::Issues {
            data,
            project,
            json,
        } => {
            let issues = store::issues(&store::open(&data)?, project.as_deref())?;
            if json {
                println!("{}", serde_json::to_string(&issues)?);
            } else {
                for i in issues {
                    println!(
                        "{}\t{}\t{}\t{}\t{}",
                        i.id,
                        i.status,
                        i.count,
                        i.title,
                        i.culprit.unwrap_or_default()
                    );
                }
            }
        }
        Command::Event { issue_id, data } => println!(
            "{}",
            serde_json::to_string_pretty(&store::payload(&store::open(&data)?, issue_id)?)?
        ),
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
            let mut hits = search(
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
            hits.reverse();
            for hit in hits {
                if json {
                    println!("{}", serde_json::to_string(&hit)?);
                    continue;
                }
                let mut line = format!(
                    "{} {:<5} {} {}",
                    hit.timestamp,
                    hit.level.as_str(),
                    hit.service.as_deref().unwrap_or("-"),
                    hit.message
                );
                for (k, v) in &hit.attributes {
                    match v {
                        serde_json::Value::String(s) => line.push_str(&format!(" {k}={s}")),
                        other => line.push_str(&format!(" {k}={other}")),
                    }
                }
                println!("{line}");
            }
        }
    }
    Ok(())
}
