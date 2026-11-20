//! Install by prompt: `davy setup` prepares a machine, `davy doctor` checks
//! it and `davy mcp-config` connects a coding agent to it.
//! Each command prints what it found or wrote and names the next step, so a
//! coding agent can finish an install without a browser or a terminal prompt.

pub mod client;
pub mod doctor;
pub mod setup;
mod unit;

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use std::path::Path;
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
        serde_json::from_str(&text)
            .with_context(|| format!("{} is not valid, run `davy setup` again", path.display()))
    }

    pub fn public_url(&self) -> anyhow::Result<Url> {
        let url = Url::parse(&self.public_url)
            .with_context(|| format!("public URL {:?} is not a URL", self.public_url))?;
        check_public_url(&url)?;
        Ok(url)
    }
}

/// Refuses a public URL that SDKs could not send to.
pub fn check_public_url(url: &Url) -> anyhow::Result<()> {
    anyhow::ensure!(
        matches!(url.scheme(), "http" | "https") && url.host_str().is_some(),
        "the public URL must start with https:// or http:// and name a host, e.g. https://davy.example.com"
    );
    Ok(())
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
}
