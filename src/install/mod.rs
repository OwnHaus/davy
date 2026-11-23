//! Install by prompt: `davy setup` prepares a machine, `davy doctor` checks
//! it and `davy mcp-config` connects a coding agent to it.
//! Each command prints what it found or wrote and names the next step, so a
//! coding agent can finish an install without a browser or a terminal prompt.

pub mod client;
pub mod doctor;
pub mod setup;
pub mod unit;

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use url::Url;

/// The admin token, one line, mode 0600.
pub const TOKEN_FILE: &str = "token";
/// The SMTP URL with its password, one line, written by the operator.
pub const SMTP_FILE: &str = "smtp-url";
/// What `davy setup` was given, read back by `doctor`, `mcp-config` and
/// `project create`.
pub const SETTINGS_FILE: &str = "setup.json";

/// The flags of the last `davy setup` in a data directory.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub public_url: String,
    pub listen: String,
    pub mail_from: Option<String>,
}

impl Settings {
    pub fn load(data: &Path) -> anyhow::Result<Self> {
        let path = data.join(SETTINGS_FILE);
        let text = std::fs::read_to_string(&path).with_context(|| {
            format!(
                "cannot read {}, run `davy setup --data {} --public-url URL` first",
                path.display(),
                sh(&data.to_string_lossy())
            )
        })?;
        let settings: Self = serde_json::from_str(&text)
            .with_context(|| format!("{} is not valid, run `davy setup` again", path.display()))?;
        anyhow::ensure!(
            settings.listen.parse::<std::net::SocketAddr>().is_ok(),
            "the listen address in {} is not an address and port, run `davy setup` again",
            path.display()
        );
        Ok(settings)
    }

    pub fn public_url(&self) -> anyhow::Result<Url> {
        let url = Url::parse(&self.public_url)
            .context("the public URL in setup.json is not a URL, run `davy setup` again")?;
        check_public_url(&url)?;
        Ok(url)
    }
}

/// Refuses a public URL that SDKs could not send to, and one that carries a
/// login, a query or a fragment, which would end up in every DSN and config.
pub fn check_public_url(url: &Url) -> anyhow::Result<()> {
    anyhow::ensure!(
        matches!(url.scheme(), "http" | "https") && url.host_str().is_some(),
        "the public URL must start with https:// or http:// and name a host, e.g. https://davy.example.com"
    );
    anyhow::ensure!(
        url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "the public URL must not hold a login, a query or a fragment, give only the scheme, host and path, e.g. https://davy.example.com"
    );
    Ok(())
}

/// The DSN of project `id` with public key `key`: the public URL with the
/// key as its user name and the project id as its path.
pub fn dsn(public_url: &Url, key: &str, id: i64) -> anyhow::Result<Url> {
    let mut dsn = public_url.clone();
    let cannot_carry_key = |()| anyhow::anyhow!("public URL cannot carry a key");
    dsn.set_username(key).map_err(cannot_carry_key)?;
    dsn.set_password(None).map_err(cannot_carry_key)?;
    dsn.set_path(&format!("/{id}"));
    dsn.set_query(None);
    dsn.set_fragment(None);
    Ok(dsn)
}

/// The path of this binary, for the service file and the printed commands.
pub fn exe() -> anyhow::Result<PathBuf> {
    std::env::current_exe()
        .and_then(|e| e.canonicalize())
        .context("cannot find the path of this binary")
}

/// Whether a token sent to `url` stays off the network in plain text:
/// https, or plain http to a loopback address.
pub fn encrypted_or_local(url: &Url) -> bool {
    url.scheme() == "https" || loopback(url)
}

/// Whether `url` names this machine by a loopback address, where plain http
/// does not leave it. A name such as `localhost` does not count, because a
/// resolver can send it to DNS and get an address elsewhere.
pub fn loopback(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(_)) | None => false,
    }
}

/// Quotes `s` for a POSIX shell, so a printed command can be pasted as is.
pub fn sh(s: &str) -> String {
    let plain = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._:@=+-,%".contains(c));
    if plain {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_printed_command_quotes_what_the_shell_would_split_or_expand() {
        assert_eq!(sh("/var/lib/davy"), "/var/lib/davy");
        assert_eq!(sh("https://davy.example.com/"), "https://davy.example.com/");
        assert_eq!(sh("/srv/my data"), "'/srv/my data'");
        assert_eq!(
            sh("Davy <alerts@example.com>"),
            "'Davy <alerts@example.com>'"
        );
        assert_eq!(sh("it's $HOME"), r"'it'\''s $HOME'");
        assert_eq!(sh(""), "''");
    }

    #[test]
    fn a_public_url_holds_no_login_query_or_fragment() {
        let check = |u: &str| check_public_url(&Url::parse(u).unwrap()).is_ok();
        assert!(check("https://davy.example.com/"));
        assert!(check("http://127.0.0.1:8000/davy/"));
        for bad in [
            "ftp://davy.example.com/",
            "https://user:pass@davy.example.com/",
            "https://davy.example.com/?key=secret",
            "https://davy.example.com/#x",
        ] {
            assert!(!check(bad), "{bad}");
        }
    }
}
