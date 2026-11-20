use clap::{Parser, Subcommand};
use davy::{http, store};
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
        } => {
            validate_url(&public_url)?;
            http::serve(&data, &listen).await?;
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
    }
    Ok(())
}
