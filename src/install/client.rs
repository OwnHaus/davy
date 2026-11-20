//! `davy mcp-config` prints how to connect a coding agent to `/mcp`. The
//! config refers to the token through `DAVY_TOKEN`, so it can be committed.

use super::{Settings, TOKEN_FILE, check_public_url, loopback};
use anyhow::Context as _;
use std::io::Write;
use std::path::PathBuf;
use url::Url;

#[derive(clap::Args)]
pub struct Args {
    #[arg(long, value_enum)]
    pub client: Client,
    /// The data directory, to read the public URL and the token from.
    #[arg(long, required_unless_present = "url")]
    pub data: Option<PathBuf>,
    /// The public URL, on a machine without the data directory.
    #[arg(long, conflicts_with = "data")]
    pub url: Option<Url>,
    /// Puts the token itself into the config instead of a reference to DAVY_TOKEN.
    #[arg(long, requires = "data")]
    pub show_token: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, clap::ValueEnum)]
pub enum Client {
    ClaudeCode,
    Codex,
    Cursor,
}

pub fn run(out: &mut impl Write, args: &Args) -> anyhow::Result<()> {
    let url = match (&args.url, &args.data) {
        (Some(url), _) => {
            check_public_url(url)?;
            url.clone()
        }
        (None, Some(data)) => Settings::load(data)?.public_url()?,
        (None, None) => anyhow::bail!("pass --data DIR or --url URL"),
    };
    let token = match (&args.data, args.show_token) {
        (Some(data), true) => {
            let path = data.join(TOKEN_FILE);
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("cannot read {}, run davy setup first", path.display()))?;
            Token::Literal(text.trim_end_matches(['\n', '\r']).to_owned())
        }
        _ => Token::Env,
    };
    let source = args.data.as_ref().map_or_else(
        || "the token file of the server".to_owned(),
        |d| d.join(TOKEN_FILE).display().to_string(),
    );
    write!(out, "{}", config(args.client, &url, &token, &source)?)?;
    Ok(())
}

/// How the config carries the token.
pub enum Token {
    /// A reference to `DAVY_TOKEN`, expanded by the client at start.
    Env,
    /// The token itself, for clients started without that variable.
    Literal(String),
}

/// What to add where for `client`. `source` names where the token is kept.
pub fn config(client: Client, url: &Url, token: &Token, source: &str) -> anyhow::Result<String> {
    anyhow::ensure!(
        url.scheme() == "https" || loopback(url),
        "{url} is plain http, so the agent would send the admin token unencrypted, put Davy behind https first (docs/install.md, step 7)"
    );
    let mcp = url.join("mcp")?;
    let env_line = match token {
        Token::Env => format!(
            "Set DAVY_TOKEN, in the environment the client starts from, to the contents of {source}.\n"
        ),
        Token::Literal(_) => {
            "This config holds the admin token. Keep the file out of version control.\n".to_owned()
        }
    };
    let bearer = |reference: &str| match token {
        Token::Env => format!("Bearer {reference}"),
        Token::Literal(t) => format!("Bearer {t}"),
    };
    let json = |file: &str, kind: Option<&'static str>, reference: &str| {
        #[derive(serde::Serialize)]
        struct Doc<'a> {
            #[serde(rename = "mcpServers")]
            mcp_servers: Servers<'a>,
        }
        #[derive(serde::Serialize)]
        struct Servers<'a> {
            davy: Server<'a>,
        }
        #[derive(serde::Serialize)]
        struct Server<'a> {
            #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
            kind: Option<&'static str>,
            url: &'a str,
            headers: Headers,
        }
        #[derive(serde::Serialize)]
        struct Headers {
            #[serde(rename = "Authorization")]
            authorization: String,
        }
        let doc = Doc {
            mcp_servers: Servers {
                davy: Server {
                    kind,
                    url: mcp.as_str(),
                    headers: Headers {
                        authorization: bearer(reference),
                    },
                },
            },
        };
        anyhow::Ok(format!(
            "Merge this into {file}, next to the servers already there.\n{env_line}\n{}\n",
            serde_json::to_string_pretty(&doc)?
        ))
    };
    match client {
        Client::ClaudeCode => json(
            ".mcp.json in the project root",
            Some("http"),
            "${DAVY_TOKEN}",
        ),
        Client::Cursor => json(
            ".cursor/mcp.json in the project root",
            None,
            "${env:DAVY_TOKEN}",
        ),
        Client::Codex => {
            let auth = match token {
                Token::Env => "bearer_token_env_var = \"DAVY_TOKEN\"".to_owned(),
                Token::Literal(t) => format!(
                    "http_headers = {{ \"Authorization\" = {} }}",
                    serde_json::to_string(&format!("Bearer {t}"))?
                ),
            };
            Ok(format!(
                "Append this to ~/.codex/config.toml.\n{env_line}\n[mcp_servers.davy]\nurl = \"{mcp}\"\n{auth}\n"
            ))
        }
    }
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;
