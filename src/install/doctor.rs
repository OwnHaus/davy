//! `davy doctor` checks a running install. Each check that is not `ok` says
//! what fixes it. It reaches only the public URL and the SMTP server.

use super::{SMTP_FILE, Settings, TOKEN_FILE, encrypted_or_local, loopback, sh};
use crate::{
    alert::mail::Smtp,
    retention,
    secret::{self, MIN_TOKEN_CHARS},
    text,
    usage::{self, forecast::Forecast},
};
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
    /// The server's default log retention, as given to `davy serve`, which
    /// doctor cannot read from a running server. The disk forecast uses it
    /// for projects without a retention of their own.
    #[arg(long, default_value_t = retention::DEFAULT_LOG_DAYS)]
    pub log_retention_days: u32,
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
    let defaults = retention::Defaults {
        logs: retention::Days::new(args.log_retention_days).map_err(anyhow::Error::msg)?,
        ..retention::Defaults::default()
    };
    let checks = checks(&args.data, defaults).await;
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

/// Runs every check against the data directory `data`, with `defaults`
/// the retention of the server that serves it.
pub async fn checks(data: &Path, defaults: retention::Defaults) -> Vec<Check> {
    let d = sh(&data.to_string_lossy());
    let mut checks = vec![data_dir(data, &d, current_user())];
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
    checks.push(logs(&crate::logs::dir(data)));
    checks.push(disk(
        data,
        fs4::available_space(data).ok(),
        forecast(data, defaults),
    ));
    checks
}

/// Entries of the log directory `root` named like a day that Davy does not
/// search, write or delete, such as a day moved to another disk and linked
/// back. Logs of such a day fail to write.
fn logs(root: &Path) -> Check {
    let r = sh(&root.to_string_lossy());
    match crate::logs::store::strays(root) {
        Err(e) => not_ok(
            "logs",
            Status::Warn,
            format!("{e:#}"),
            format!("make {r} readable by the user Davy runs as"),
        ),
        Ok(strays) if strays.is_empty() => {
            ok("logs", "every day of logs is a directory of its own")
        }
        Ok(strays) => {
            let each: Vec<String> = strays
                .iter()
                .map(|s| format!("{:?} {}", s.name, s.why))
                .collect();
            not_ok(
                "logs",
                Status::Warn,
                format!("not searched, written or deleted: {}", each.join("; ")),
                format!("move each out of {r}, or put the day's own directory in its place"),
            )
        }
    }
}

/// `me` is the user doctor runs as. Another user who owns the directory
/// can read and change everything in it, whatever its mode says.
fn data_dir(data: &Path, d: &str, me: u32) -> Check {
    if !data.is_dir() {
        return not_ok(
            "data",
            Status::Fail,
            format!("{d} does not exist"),
            format!("davy setup --data {d} --public-url URL"),
        );
    }
    if let Some(owner) = other_owner(data, me) {
        return not_ok(
            "data",
            Status::Warn,
            format!("{d} belongs to user {owner}, and doctor runs as user {me}"),
            "run Davy and doctor as the user that owns the data directory (docs/install.md)",
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
    let chars = secret::line(token).chars().count();
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
    if !encrypted_or_local(url) {
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
    let reply = match post().bearer_auth(secret::line(token)).send().await {
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
    let checked = match Smtp::new(secret::line(&url), from) {
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

/// The disk forecast, `None` while the data directory has no database to
/// read. Projects without a retention of their own keep `defaults`.
fn forecast(data: &Path, defaults: retention::Defaults) -> Option<Result<Forecast, String>> {
    if !data.join(crate::store::DB_FILE).exists() {
        return None;
    }
    let request = usage::Request {
        project: None,
        since: None,
        what_if: usage::forecast::WhatIf::default(),
        defaults,
    };
    let report = crate::query::open(data)
        .and_then(|c| usage::report(&c, data, &request, crate::time::now_ns()));
    Some(
        report
            .map_err(|e| format!("{e:#}"))
            .and_then(|r| r.forecast),
    )
}

/// Free space, and when the forecast puts the disk full: a warning under
/// 14 days, a failure under 2.
fn disk(data: &Path, free: Option<u64>, forecast: Option<Result<Forecast, String>>) -> Check {
    let d = sh(&data.to_string_lossy());
    let fix = format!(
        "shorten retention with davy project retention SLUG --data {d} --logs N, or add disk"
    );
    let (status, free) = match free {
        None => (Status::Warn, "free space is unknown".to_owned()),
        Some(b) if b < GIB => (Status::Fail, format!("{} MiB free", b >> 20)),
        Some(b) if b < 5 * GIB => (Status::Warn, format!("{} GiB free", b / GIB)),
        Some(b) => (Status::Ok, format!("{} GiB free", b / GIB)),
    };
    let (by_forecast, said) = match forecast {
        None => (Status::Ok, String::new()),
        Some(Err(reason)) => (Status::Ok, format!(", no forecast: {reason}")),
        // Days left alone decide, so a day past the calendar still warns.
        Some(Ok(Forecast {
            days_left: Some(n),
            full_on,
            ..
        })) => {
            let unit = if n == 1 { "day" } else { "days" };
            let status = match n {
                0..2 => Status::Fail,
                2..14 => Status::Warn,
                _ => Status::Ok,
            };
            let on = full_on.map(|on| format!("on {on}, ")).unwrap_or_default();
            (status, format!(", forecast full {on}in {n} {unit}"))
        }
        Some(Ok(_)) => (Status::Ok, ", never fills at the current rate".to_owned()),
    };
    let status = worse(status, by_forecast);
    let detail = free + &said;
    match status {
        Status::Ok => ok("disk", detail),
        _ => not_ok("disk", status, detail, fix),
    }
}

fn worse(a: Status, b: Status) -> Status {
    let rank = |s| match s {
        Status::Ok => 0,
        Status::Warn => 1,
        Status::Fail => 2,
    };
    if rank(b) > rank(a) { b } else { a }
}

#[cfg(unix)]
fn current_user() -> u32 {
    nix::unistd::geteuid().as_raw()
}

#[cfg(not(unix))]
fn current_user() -> u32 {
    0
}

/// The owner of `path`, when that is not `me`.
#[cfg(unix)]
fn other_owner(path: &Path, me: u32) -> Option<u32> {
    use std::os::unix::fs::MetadataExt as _;
    let owner = std::fs::metadata(path).ok()?.uid();
    (owner != me).then_some(owner)
}

#[cfg(not(unix))]
fn other_owner(_path: &Path, _me: u32) -> Option<u32> {
    None
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
