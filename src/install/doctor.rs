//! `davy doctor` checks a running install. Each check that is not `ok` says
//! what fixes it. It reaches only the public URL and the SMTP server.

use super::{SMTP_FILE, Settings, TOKEN_FILE, sh};
use crate::{alert::mail::Smtp, secret::MIN_TOKEN_CHARS};
use serde::Serialize;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;
use url::{Host, Url};

const TIMEOUT: Duration = Duration::from_secs(5);
const GIB: u64 = 1 << 30;

#[derive(clap::Args)]
pub struct Args {
    #[arg(long)]
    pub data: PathBuf,
    #[arg(long)]
    pub json: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Warn,
    Fail,
}

#[derive(Debug, PartialEq, Serialize)]
pub struct Check {
    pub name: &'static str,
    pub status: Status,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

fn ok(name: &'static str, detail: impl Into<String>) -> Check {
    Check {
        name,
        status: Status::Ok,
        detail: detail.into(),
        fix: None,
    }
}

fn not_ok(
    name: &'static str,
    status: Status,
    detail: impl Into<String>,
    fix: impl Into<String>,
) -> Check {
    Check {
        name,
        status,
        detail: detail.into(),
        fix: Some(fix.into()),
    }
}

pub async fn run(out: &mut impl Write, args: &Args) -> anyhow::Result<()> {
    let checks = checks(&args.data).await;
    if args.json {
        #[derive(Serialize)]
        struct Report<'a> {
            checks: &'a [Check],
        }
        serde_json::to_writer_pretty(&mut *out, &Report { checks: &checks })?;
        writeln!(out)?;
    } else {
        for c in &checks {
            let status = format!("{:?}", c.status).to_lowercase();
            writeln!(out, "{status:<5} {:<9} {}", c.name, c.detail)?;
            if let Some(fix) = &c.fix {
                writeln!(out, "{:16}fix: {fix}", "")?;
            }
        }
    }
    let failed = checks.iter().filter(|c| c.status == Status::Fail).count();
    anyhow::ensure!(
        failed == 0,
        "{failed} of {} checks failed, run the fix under each",
        checks.len()
    );
    Ok(())
}

/// Runs every check against the data directory `data`.
pub async fn checks(data: &Path) -> Vec<Check> {
    let d = sh(&data.to_string_lossy());
    let mut checks = vec![data_dir(data, &d)];
    let token = std::fs::read_to_string(data.join(TOKEN_FILE)).ok();
    checks.push(token_file(&data.join(TOKEN_FILE), token.as_deref(), &d));
    match Settings::load(data) {
        Err(e) => checks.push(not_ok(
            "settings",
            Status::Fail,
            format!("{e:#}"),
            format!("davy setup --data {d} --public-url URL"),
        )),
        Ok(settings) => match settings.public_url() {
            Err(e) => checks.push(not_ok(
                "settings",
                Status::Fail,
                format!("{e:#}"),
                format!("davy setup --data {d} --public-url https://HOST"),
            )),
            Ok(url) => {
                checks.push(tls(&url, &settings.listen, &d));
                checks.extend(server(&url, token.as_deref(), &d).await);
                checks.push(smtp(data, &settings, &d).await);
            }
        },
    }
    checks.push(disk(data, fs4::available_space(data).ok()));
    checks
}

fn data_dir(data: &Path, d: &str) -> Check {
    if !data.is_dir() {
        return not_ok(
            "data",
            Status::Fail,
            format!("{d} does not exist"),
            format!("davy setup --data {d} --public-url URL"),
        );
    }
    match open_mode(data) {
        Some(mode) => not_ok(
            "data",
            Status::Warn,
            format!("{d} can be read by other users (mode {mode:o})"),
            format!("chmod 700 {d}"),
        ),
        None => ok("data", format!("{d} is readable by its owner only")),
    }
}

fn token_file(path: &Path, token: Option<&str>, d: &str) -> Check {
    let p = sh(&path.to_string_lossy());
    let Some(token) = token else {
        return not_ok(
            "token",
            Status::Fail,
            format!("{p} is missing"),
            format!("davy setup --data {d} --public-url URL"),
        );
    };
    let chars = token.trim_end_matches(['\n', '\r']).chars().count();
    if chars < MIN_TOKEN_CHARS {
        return not_ok(
            "token",
            Status::Fail,
            format!("{p} holds {chars} characters and needs {MIN_TOKEN_CHARS}"),
            format!(
                "rm {p} && davy setup --data {d} --public-url URL, then update every client of the old token"
            ),
        );
    }
    match open_mode(path) {
        Some(mode) => not_ok(
            "token",
            Status::Warn,
            format!("{p} can be read by other users (mode {mode:o})"),
            format!("chmod 600 {p}"),
        ),
        None => ok("token", format!("{p} holds a token of {chars} characters")),
    }
}

fn tls(url: &Url, listen: &str, d: &str) -> Check {
    if url.scheme() == "https" {
        return ok("tls", format!("{url} uses https"));
    }
    let loopback = match url.host() {
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        Some(Host::Domain(name)) => name == "localhost" || name.ends_with(".localhost"),
        None => false,
    };
    if loopback {
        return ok("tls", format!("{url} is plain http on this machine only"));
    }
    not_ok(
        "tls",
        Status::Fail,
        format!("{url} is plain http, so the admin token and events cross the network unencrypted"),
        format!(
            "put a TLS proxy such as Caddy in front of {listen}, then davy setup --data {d} --public-url https://HOST"
        ),
    )
}

/// Whether the server answers at `url`, and whether it was started with the
/// token in the data directory: `/mcp` answers 401 to another token and 404
/// when the server runs without one.
async fn server(url: &Url, token: Option<&str>, d: &str) -> Vec<Check> {
    let start = format!(
        "start the server (davy setup --data {d} printed how) and read its log; behind a proxy, check that the proxy forwards to it"
    );
    let client = match reqwest::Client::builder()
        .timeout(TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
    {
        Ok(client) => client,
        Err(e) => {
            return vec![not_ok(
                "server",
                Status::Fail,
                crate::net::error_text(e),
                start,
            )];
        }
    };
    let answered = match client.get(url.clone()).send().await {
        Ok(r) => ok(
            "server",
            format!("{url} answers (HTTP {})", r.status().as_u16()),
        ),
        Err(e) => {
            return vec![not_ok(
                "server",
                Status::Fail,
                format!("{url} does not answer: {}", crate::net::error_text(e)),
                start,
            )];
        }
    };
    let Some(token) = token else {
        return vec![answered];
    };
    let mcp = url.join("mcp").unwrap_or_else(|_| url.clone());
    let restart = "restart the server so it reads DAVY_TOKEN_FILE again";
    let check = match client
        .get(mcp.clone())
        .bearer_auth(token.trim_end_matches(['\n', '\r']))
        .send()
        .await
    {
        Ok(r) if r.status() == reqwest::StatusCode::UNAUTHORIZED => not_ok(
            "mcp",
            Status::Fail,
            format!("{mcp} refuses the token in {d}/token, so the server runs with another one"),
            restart,
        ),
        Ok(r) if r.status() == reqwest::StatusCode::NOT_FOUND => not_ok(
            "mcp",
            Status::Fail,
            format!("{mcp} is not served, so the server runs without an admin token"),
            format!("set DAVY_TOKEN_FILE={d}/token in the server's environment and restart it"),
        ),
        Ok(_) => ok("mcp", format!("{mcp} accepts the token")),
        Err(e) => not_ok("mcp", Status::Fail, crate::net::error_text(e), restart),
    };
    vec![answered, check]
}

async fn smtp(data: &Path, settings: &Settings, d: &str) -> Check {
    let Ok(url) = std::fs::read_to_string(data.join(SMTP_FILE)) else {
        return ok(
            "smtp",
            "no SMTP server set, alerts go to webhooks and Slack only",
        );
    };
    let fix = format!(
        "check the host, port and login in {d}/{SMTP_FILE}, then davy setup again and restart the server"
    );
    let Some(from) = &settings.mail_from else {
        return not_ok(
            "smtp",
            Status::Fail,
            format!("{d}/{SMTP_FILE} is set and --mail-from is not"),
            format!("davy setup --data {d} --public-url URL --mail-from ADDRESS"),
        );
    };
    let checked = match Smtp::new(url.trim_end_matches(['\n', '\r']), from) {
        Ok(smtp) => smtp.check().await,
        Err(e) => Err(format!("{e:#}")),
    };
    match checked {
        Ok(()) => ok("smtp", "the SMTP server accepts a connection"),
        Err(e) => not_ok(
            "smtp",
            Status::Fail,
            format!("the SMTP server refused: {e}"),
            fix,
        ),
    }
}

fn disk(data: &Path, free: Option<u64>) -> Check {
    let fix = format!(
        "free space on the disk of {}, or lower --log-retention-days",
        sh(&data.to_string_lossy())
    );
    match free {
        None => not_ok("disk", Status::Warn, "free space is unknown", fix),
        Some(b) if b < GIB => not_ok("disk", Status::Fail, format!("{} MiB free", b >> 20), fix),
        Some(b) if b < 5 * GIB => {
            not_ok("disk", Status::Warn, format!("{} GiB free", b / GIB), fix)
        }
        Some(b) => ok("disk", format!("{} GiB free", b / GIB)),
    }
}

/// The permission bits that let other users in, when there are any.
#[cfg(unix)]
fn open_mode(path: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = std::fs::metadata(path).ok()?.permissions().mode() & 0o777;
    (mode & 0o077 != 0).then_some(mode)
}

#[cfg(not(unix))]
fn open_mode(_path: &Path) -> Option<u32> {
    None
}

#[cfg(test)]
#[path = "doctor_tests.rs"]
mod tests;
