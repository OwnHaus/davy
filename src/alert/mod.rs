//! Alert channels: where notices about issues and incidents go.

mod notice;
pub mod store;
#[cfg(test)]
mod tests;

pub use notice::{Incident, Issue, Notice};

use anyhow::anyhow;

/// A webhook gets a signed JSON body, Slack an incoming webhook message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Webhook,
    Slack,
}

impl Kind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Webhook => "webhook",
            Self::Slack => "slack",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        [Self::Webhook, Self::Slack]
            .into_iter()
            .find(|kind| kind.as_str() == s)
    }
}

/// 32 random bytes as 64 lowercase hex characters. A webhook receiver checks
/// the signature of each delivery with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    fn random() -> anyhow::Result<Self> {
        let mut bytes = [0_u8; 32];
        getrandom::fill(&mut bytes).map_err(|e| anyhow!("no randomness for a secret: {e}"))?;
        let hex = bytes
            .iter()
            .flat_map(|b| [b >> 4, b & 0xf])
            .filter_map(|nibble| char::from_digit(u32::from(nibble), 16))
            .collect();
        Ok(Self(hex))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
