//! Policies people send, checked before they are stored.
use super::{Policy, keys};
use anyhow::{Result, bail, ensure};
use serde::Deserialize;

/// Extra keys one policy may hold.
pub const MAX_EXTRA_KEYS: usize = 100;
/// Bytes one extra key may hold.
pub const MAX_KEY_BYTES: usize = 100;

/// A policy as sent, which names no field Davy does not know.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    keys: Option<bool>,
    credentials: Option<bool>,
    cards: Option<bool>,
    email: Option<bool>,
    personal: Option<bool>,
    ip: Option<bool>,
    extra_keys: Option<Vec<String>>,
}

/// What a field the input leaves out means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Missing {
    /// Refused, so that a client that knows fewer fields cannot turn the
    /// others back to their defaults.
    Refuse,
    /// The default, for the CLI.
    Default,
}

/// Reads a policy sent by a person or an agent.
pub fn parse(json: &[u8], missing: Missing) -> Result<Policy> {
    let input: Input = serde_json::from_slice(json)?;
    let defaults = Policy::default();
    let mut absent = Vec::new();
    let mut field = |value: Option<bool>, name: &'static str, default: bool| {
        value.unwrap_or_else(|| {
            absent.push(name);
            default
        })
    };
    let mut policy = Policy {
        keys: field(input.keys, "keys", defaults.keys),
        credentials: field(input.credentials, "credentials", defaults.credentials),
        cards: field(input.cards, "cards", defaults.cards),
        email: field(input.email, "email", defaults.email),
        personal: field(input.personal, "personal", defaults.personal),
        ip: field(input.ip, "ip", defaults.ip),
        extra_keys: defaults.extra_keys,
    };
    if input.extra_keys.is_none() {
        absent.push("extra_keys");
    }
    if missing == Missing::Refuse && !absent.is_empty() {
        bail!(
            "a policy must name every field, and this one leaves out {}",
            absent.join(", ")
        );
    }
    let extra = input.extra_keys.unwrap_or_default();
    ensure!(
        extra.len() <= MAX_EXTRA_KEYS,
        "at most {MAX_EXTRA_KEYS} extra keys, not {}",
        extra.len()
    );
    for key in &extra {
        ensure!(
            (1..=MAX_KEY_BYTES).contains(&key.len()),
            "extra key {key:?} must hold 1 to {MAX_KEY_BYTES} bytes"
        );
        ensure!(
            key.bytes().any(|b| b.is_ascii_alphanumeric()),
            "extra key {key:?} has no ASCII letter or digit, and keys are compared by those alone"
        );
        ensure!(
            !key.contains(['%', '+']),
            "extra key {key:?} holds % or +; give the decoded name, as form-encoded keys in text are decoded before they are compared"
        );
    }
    policy.extra_keys = extra.iter().map(|key| keys::normalized(key)).collect();
    Ok(policy)
}
