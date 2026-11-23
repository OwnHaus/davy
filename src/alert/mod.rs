//! Alert channels: where notices about issues and incidents go.

pub mod deliver;
#[cfg(test)]
mod deliver_tests;
pub mod mail;
#[cfg(test)]
mod mail_tests;
mod notice;
pub mod store;
#[cfg(test)]
mod tests;

pub use notice::{Incident, Issue, Mail, Notice};

use crate::{monitor::CheckUrl, secret, text::hex};
use anyhow::anyhow;
use hmac::{Hmac, KeyInit as _, Mac as _};
use sha2::Sha256;
use std::time::Duration;

const MAX_ADDRESS: usize = 254;
/// How long one webhook or SMTP delivery may take.
const SEND_TIMEOUT: Duration = Duration::from_secs(10);

/// A webhook gets a signed JSON body, Slack an incoming webhook message and
/// an e-mail channel a plain text message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Webhook,
    Slack,
    Email,
}

impl Kind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Webhook => "webhook",
            Self::Slack => "slack",
            Self::Email => "email",
        }
    }
}

/// One `user@domain` address of at most 254 bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmailAddress(lettre::Address);

impl EmailAddress {
    pub fn parse(s: &str) -> Result<Self, String> {
        if s.len() > MAX_ADDRESS {
            return Err(format!("the address is longer than {MAX_ADDRESS} bytes"));
        }
        s.parse()
            .map(Self)
            .map_err(|e| format!("the address is not valid: {e}"))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_ref()
    }
}

/// Where a new channel sends its notices.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Destination {
    Webhook(CheckUrl),
    Slack(CheckUrl),
    Email(EmailAddress),
}

impl Destination {
    #[must_use]
    pub fn kind(&self) -> Kind {
        match self {
            Self::Webhook(_) => Kind::Webhook,
            Self::Slack(_) => Kind::Slack,
            Self::Email(_) => Kind::Email,
        }
    }

    /// The `url` column of the channel row. An address becomes a `mailto:`
    /// URL.
    fn url(&self) -> String {
        match self {
            Self::Webhook(url) | Self::Slack(url) => url.as_str().to_owned(),
            Self::Email(address) => format!("mailto:{}", address.as_str()),
        }
    }
}

/// Where a notification goes. Both URLs are credentials. An address is not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Channel {
    Webhook { url: String, secret: Secret },
    Slack { url: String },
    Email { to: String },
}

/// 32 random bytes as 64 lowercase hex characters. A webhook receiver checks
/// the signature of each delivery with it.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(..)")
    }
}

impl Secret {
    fn random() -> anyhow::Result<Self> {
        let hex = secret::random_hex().map_err(|e| anyhow!("no randomness for a secret: {e}"))?;
        Ok(Self(hex))
    }

    /// A secret read back from its channel row.
    #[must_use]
    pub fn load(s: String) -> Self {
        Self(s)
    }

    /// The `Davy-Signature` header of `body` sent at unix second `t`, the
    /// scheme Stripe uses. The key is the secret's 64 characters as text.
    pub fn sign(&self, t: i64, body: &str) -> anyhow::Result<String> {
        let mut mac = Hmac::<Sha256>::new_from_slice(self.0.as_bytes())
            .map_err(|e| anyhow!("HMAC refused the secret as a key: {e}"))?;
        mac.update(format!("{t}.").as_bytes());
        mac.update(body.as_bytes());
        Ok(format!("t={t},v1={}", hex(&mac.finalize().into_bytes())))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// How long to wait after the `failures`-th failed delivery of a row, or
/// `None` when the row has failed for good: 30 s, then doubling to 8 min.
#[must_use]
pub fn retry_after(failures: u32) -> Option<Duration> {
    let doublings = failures.checked_sub(1).filter(|&d| d < 5)?;
    Some(Duration::from_secs(30 << doublings))
}
