//! `davy doctor` checks a running install. Each check that is not `ok` says
//! what fixes it. It reaches only the public URL and the SMTP server.

use super::{SMTP_FILE, Settings, TOKEN_FILE, loopback, sh};
use crate::{alert::mail::Smtp, secret::MIN_TOKEN_CHARS, text};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::Duration;
use std::{borrow::Cow, io::Write};
use url::Url;

const TIMEOUT: Duration = Duration::from_secs(5);
/// Davy's `initialize` reply is under 1 KiB.
const MAX_REPLY: usize = 64 * 1024;
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
        detail: printable(detail.into()),
        fix: None,
    }
}

/// An SMTP server or a path can put control characters into a check, and
/// they would move the cursor or retitle the terminal that prints it.
fn printable(s: String) -> String {
    match text::printable(&s, char::REPLACEMENT_CHARACTER) {
        Cow::Owned(clean) => clean,
        Cow::Borrowed(_) => s,
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
        detail: printable(detail.into()),
        fix: Some(printable(fix.into())),
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
    if loopback(url) {
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

/// The `initialize` request an MCP client sends first.
const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"davy-doctor","version":"1"}}}"#;

/// Whether the server answers at `url`, and whether its `/mcp` is Davy's,
/// started with the token in the data directory. The token is sent only over
/// https or to this machine.
async fn server(url: &Url, token: Option<&str>, d: &str) -> Vec<Check> {
    let start = format!(
        "start the server (davy setup --data {d} printed how) and read its log; behind a proxy, check that the proxy forwards to it"
    );
    let mut builder = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .redirect(reqwest::redirect::Policy::none());
    if loopback(url) {
        builder = builder.no_proxy();
    }
    let client = match builder.build() {
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
    vec![answered, mcp(&client, url, token, d).await]
}

async fn mcp(client: &reqwest::Client, url: &Url, token: &str, d: &str) -> Check {
    use reqwest::{StatusCode, header};
    let mcp = url.join("mcp").unwrap_or_else(|_| url.clone());
    if url.scheme() != "https" && !loopback(url) {
        return not_ok(
            "mcp",
            Status::Warn,
            format!("{mcp} was not checked, so the token does not cross the network unencrypted"),
            "serve Davy over https, see the tls check",
        );
    }
    let restart = format!("restart the server, it reads {d}/token when it starts");
    let post = || {
        client
            .post(mcp.clone())
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json, text/event-stream")
            .body(INITIALIZE)
    };
    match post().send().await {
        Ok(r) if r.status() == StatusCode::UNAUTHORIZED => {}
        Ok(r) if r.status() == StatusCode::NOT_FOUND => {
            return not_ok(
                "mcp",
                Status::Fail,
                format!("{mcp} is not served, so the server runs without an admin token"),
                restart,
            );
        }
        Ok(r) => {
            return not_ok(
                "mcp",
                Status::Fail,
                format!(
                    "{mcp} answers HTTP {} without a token, so something other than Davy serves it",
                    r.status().as_u16()
                ),
                "check that the proxy forwards /mcp to Davy unchanged",
            );
        }
        Err(e) => return not_ok("mcp", Status::Fail, crate::net::error_text(e), restart),
    }
    let reply = match post()
        .bearer_auth(token.trim_end_matches(['\n', '\r']))
        .send()
        .await
    {
        Ok(r) => (r.status(), initialized(r).await),
        Err(e) => return not_ok("mcp", Status::Fail, crate::net::error_text(e), restart),
    };
    match reply {
        (StatusCode::OK, true) => ok("mcp", format!("{mcp} accepts the token")),
        (StatusCode::UNAUTHORIZED, _) => not_ok(
            "mcp",
            Status::Fail,
            format!("{mcp} refuses the token in {d}/token, so the server runs with another one"),
            restart,
        ),
        (status, _) => not_ok(
            "mcp",
            Status::Fail,
            format!(
                "{mcp} answers HTTP {} to the token without an MCP reply",
                status.as_u16()
            ),
            "check that the proxy forwards /mcp to Davy unchanged, then read the server log",
        ),
    }
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

/// Whether `r` is the JSON-RPC reply to [`INITIALIZE`], read up to
/// [`MAX_REPLY`] bytes.
async fn initialized(mut r: reqwest::Response) -> bool {
    let mut body = Vec::new();
    while let Ok(Some(chunk)) = r.chunk().await {
        if body.len() + chunk.len() > MAX_REPLY {
            return false;
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice::<serde_json::Value>(&body).is_ok_and(|v| {
        v["jsonrpc"] == "2.0" && v["id"] == 1 && v["result"]["serverInfo"]["name"].is_string()
    })
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
