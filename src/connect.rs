//! Connection instructions shared by the local CLI and the admin MCP reader.
use crate::{install, logs, query, retention, store, time};
use rusqlite::OptionalExtension as _;
use serde::Serialize;
use std::{
    io::Write,
    net::SocketAddr,
    path::{Path, PathBuf},
};
use url::Url;

mod sdk;
pub use sdk::Sdk;

#[derive(clap::Args)]
pub struct Args {
    /// The project slug to create or reuse.
    pub slug: String,
    #[arg(long)]
    pub data: PathBuf,
    /// Print install and init instructions for this SDK.
    #[arg(long, value_enum)]
    pub sdk: Option<Sdk>,
    /// Use the loopback listener from DATA/setup.json, for apps on this host.
    #[arg(long)]
    pub local: bool,
}

#[derive(Debug, Serialize)]
pub struct Project {
    pub id: i64,
    pub slug: String,
    pub dsn: String,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub project: Project,
    pub supported_sdks: &'static [&'static str],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sdk: Option<sdk::Instructions>,
    pub test_event: String,
}

pub fn run(out: &mut impl Write, args: &Args) -> anyhow::Result<()> {
    let saved = install::Settings::load(&args.data)?;
    let base = if args.local {
        local_url(&saved.listen)?
    } else {
        saved.public_url()?
    };
    validate_slug(&args.slug)?;
    store::create_project(&store::open(&args.data)?, &args.slug)?;
    serde_json::to_writer_pretty(
        &mut *out,
        &existing(&args.data, &args.slug, &base, args.sdk)?,
    )?;
    writeln!(out)?;
    Ok(())
}

fn validate_slug(slug: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !slug.trim().is_empty() && slug.len() <= 200,
        "slug must contain 1 to 200 bytes"
    );
    Ok(())
}

pub fn local_url(listen: &str) -> anyhow::Result<Url> {
    let mut addr: SocketAddr = listen.parse()?;
    if addr.ip().is_unspecified() {
        addr.set_ip(if addr.is_ipv4() {
            std::net::Ipv4Addr::LOCALHOST.into()
        } else {
            std::net::Ipv6Addr::LOCALHOST.into()
        });
    }
    anyhow::ensure!(
        addr.ip().is_loopback() && addr.port() != 0,
        "local DSN needs a loopback or wildcard listener with a fixed port; run davy setup --listen ADDRESS:PORT"
    );
    Ok(Url::parse(&format!("http://{addr}"))?)
}

/// Reads a project without creating a key or changing the store.
pub fn existing(data: &Path, slug: &str, base: &Url, sdk: Option<Sdk>) -> anyhow::Result<Report> {
    validate_slug(slug)?;
    install::check_public_url(base)?;
    let c = query::open(data)?;
    let found = c.query_row(
        "SELECT id, public_key FROM projects WHERE slug=?1",
        [slug],
        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
    );
    let (id, key) = found.optional()?.ok_or_else(|| {
        anyhow::anyhow!(
            "project not found; run davy connect {} --data DATA on the server",
            install::sh(slug)
        )
    })?;
    let mut endpoint = base.clone();
    endpoint.set_path(&format!("/api/{id}/store/"));
    endpoint.query_pairs_mut().append_pair("sentry_key", &key);
    let test_event = format!(
        "curl --fail-with-body -sS -H 'Content-Type: application/json' --data '{{\"message\":\"Davy test error\",\"level\":\"error\"}}' {}",
        install::sh(endpoint.as_str())
    );
    Ok(Report {
        project: Project {
            id,
            slug: slug.to_owned(),
            dsn: install::dsn(base, &key, id)?.to_string(),
        },
        supported_sdks: &[
            "android", "rust", "go", "node", "python", "dotnet", "browser",
        ],
        sdk: sdk.map(Sdk::instructions),
        test_event,
    })
}

pub fn list(out: &mut impl Write, data: &Path) -> anyhow::Result<()> {
    let c = query::open(data)?;
    let mut q = c.prepare("SELECT id, slug FROM projects ORDER BY id")?;
    let rows = q
        .query_map([], |r| {
            Ok(serde_json::json!({"id": r.get::<_, i64>(0)?, "slug": r.get::<_, String>(1)?}))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    serde_json::to_writer_pretty(&mut *out, &rows)?;
    writeln!(out)?;
    Ok(())
}

#[derive(Serialize)]
pub struct Status {
    pub scrub: crate::scrub::Policy,
    /// Events the project keeps.
    pub event_count: i64,
    /// RFC 3339, null before the first event.
    pub last_event_time: Option<String>,
    pub issue_count: i64,
    /// RFC 3339, null before the first log line.
    pub last_log_time: Option<String>,
    /// How long the project keeps its logs and errors.
    pub retention: retention::Retention,
}

/// Whether the project `slug` has sent anything yet, and how long it keeps
/// it, where projects without a setting keep what `defaults` says.
pub fn status(data: &Path, slug: &str, defaults: retention::Defaults) -> anyhow::Result<Status> {
    let c = query::open(data)?;
    let id = store::project_by_slug(&c, slug)?;
    let retention = retention::one(&c, slug, defaults, time::now_ms())?
        .ok_or_else(|| anyhow::anyhow!(retention::unknown(slug)))?;
    // events has no index by project, so the counts come from the issues
    // index, which holds event_count and last_seen.
    let (issue_count, event_count, last_seen) = c.query_row(
        "SELECT count(*), coalesce(sum(event_count), 0), max(last_seen) FROM issues WHERE project_id=?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, Option<i64>>(2)?)),
    )?;
    let q = logs::store::LogQuery {
        project: Some(id),
        since: i64::MIN,
        until: time::now_ns(),
        min_level: None,
        text: None,
        pattern: None,
        limit: 1,
        skip: 0,
    };
    let last_log_time = logs::store::search(&logs::dir(data), &q)?
        .first()
        .map(|h| time::rfc3339(h.timestamp_ns));
    Ok(Status {
        scrub: crate::scrub::settings::read(&c, id)?,
        event_count,
        last_event_time: last_seen.map(time::rfc3339_ms),
        issue_count,
        last_log_time,
        retention,
    })
}

#[cfg(test)]
#[path = "connect_tests.rs"]
mod tests;
