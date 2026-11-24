//! Values recognized by their shape, wherever they appear in text.
use super::{
    Policy,
    meter::{self, Meter, Rewrite},
};
use regex::Regex;
use std::{
    net::{IpAddr, Ipv4Addr},
    sync::LazyLock,
};

pub(super) const CREDENTIAL_MARKER: &str = "[Filtered: credential]";
pub(super) const IP_MARKER: &str = "[Filtered: ip]";
const PRIVATE_MARKER: &str = "[Filtered: private key]";
/// What a string with more sensitive assignments than
/// [`MAX_ASSIGNMENTS`] becomes, whole.
/// What [`key_marker`] renames a key to.
pub(super) const KEY_MARKERS: [&str; 4] = [CREDENTIAL_MARKER, super::EMAIL, IP_MARKER, super::CARD];
pub(super) const TOO_MANY: &str = "[Filtered: too many assignments]";
/// Sensitive assignments rewritten in one string. Past this a string is
/// replaced whole, so its cost and growth stay bounded.
pub(super) const MAX_ASSIGNMENTS: usize = 256;
const ASSIGNED: &str = "=[Filtered: sensitive]";

fn regex(pattern: &str) -> Regex {
    match Regex::new(pattern) {
        Ok(regex) => regex,
        Err(error) => unreachable!("fixed scrub pattern: {error}"),
    }
}
static CREDENTIAL: LazyLock<Regex> = LazyLock::new(|| {
    regex(concat!(
        r"(?i)(?-u:\b)(?:[a-z][a-z0-9+.-]*://[^/\s:@]+:[^/\s@]+@|bearer\s+[a-z0-9._~+/=-]+|",
        r"(?:gh[pousr]_|github_pat_)[a-z0-9_]+|",
        r"sk-(?:ant-|proj-|svcacct-)?[a-z0-9_-]{16,}|",
        r"(?:AKIA|ASIA)[A-Z0-9]{16}|(?:xox[baprs]|xapp)-[a-z0-9-]+|",
        r"(?:sk|rk|pk)_(?:live|test)_[a-z0-9]+|",
        r"(?-i:AIza)[0-9a-z_-]{35}|(?-i:glpat-)[0-9a-z_-]{20,}|(?-i:npm_)[a-z0-9]{36}|",
        r"whsec_[a-z0-9+/=]{24,}|(?-i:SG)\.[a-z0-9_-]{16,}\.[a-z0-9_-]{16,}|",
        r"eyJ[a-z0-9_-]+\.eyJ[a-z0-9_-]+(?:\.[a-z0-9_-]*)?|",
        r"eyJ[a-z0-9_-]+\.[a-z0-9_-]+\.[a-z0-9_-]+)"
    ))
});
static PRIVATE: LazyLock<Regex> = LazyLock::new(|| {
    regex(r"(?s)-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----.*?(?:-----END [A-Z0-9 ]*PRIVATE KEY-----|\z)")
});
static EMAIL: LazyLock<Regex> =
    LazyLock::new(|| regex(r"[a-zA-Z0-9.!#$%&'*+/=?^_`{|}~-]+@[a-zA-Z0-9-]+(?:\.[a-zA-Z0-9-]+)+"));
static IP: LazyLock<Regex> =
    LazyLock::new(|| regex(r"[0-9A-Fa-f:.]*[:.][0-9A-Fa-f:.]*[0-9A-Fa-f]"));
static CARD: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?-u:\b)[0-9](?:[ -]?[0-9]){12,18}(?-u:\b)"));
/// A key, then a separator. The key may close a quote, escaped as it is in
/// JSON inside a string. It may hold non-ASCII letters, which fold to ASCII
/// before it is compared.
static ASSIGN: LazyLock<Regex> =
    LazyLock::new(|| regex(r#"(?i)(?:[a-z0-9_.%\-]|[^\x00-\x7F])+\\?["']?\s*(?:=|:|%3d|%3a)\s*"#));

/// The key of an [`ASSIGN`] match, without what follows it.
fn assignment_key(matched: &str) -> &str {
    let rest = matched.trim_end();
    let rest = match rest.strip_suffix(['=', ':']) {
        Some(rest) => rest,
        None => rest.get(..rest.len().saturating_sub(3)).unwrap_or_default(),
    };
    let rest = rest.trim_end();
    let rest = rest.strip_suffix(['"', '\'']).unwrap_or(rest);
    rest.strip_suffix('\\').unwrap_or(rest)
}

/// Rewrites `key=value` to `key=[Filtered: sensitive]` where the key is
/// sensitive. False when the meter refused to pay.
fn assignments(policy: &Policy, text: &mut String, meter: &mut Meter<'_>) -> bool {
    if !text.contains(['=', ':']) && !text.contains("%3") {
        return true;
    }
    let mut rewrite = Rewrite::new(text);
    let (mut count, mut kept) = (0, 0);
    for found in ASSIGN.find_iter(text) {
        if found.start() < kept {
            continue;
        }
        let key = assignment_key(found.as_str());
        if !policy.sensitive_assignment(key) {
            continue;
        }
        count += 1;
        if count > MAX_ASSIGNMENTS {
            rewrite.abandon(meter);
            return meter::set(text, TOO_MANY, meter);
        }
        kept = found.end() + value_end(&text[found.end()..]);
        if !rewrite.replace(found.start() + key.len(), kept, ASSIGNED, meter) {
            rewrite.abandon(meter);
            return false;
        }
    }
    rewrite.finish(meter).apply(text, meter)
}

/// Where the value at the start of `text` ends: after its closing quote,
/// or at the first delimiter.
fn value_end(text: &str) -> usize {
    if text.starts_with(super::SENSITIVE) {
        return super::SENSITIVE.len();
    }
    let bytes = text.as_bytes();
    match bytes {
        [b'\\', quote @ (b'"' | b'\''), ..] => escaped_end(bytes, *quote),
        [quote @ (b'"' | b'\''), ..] => {
            let mut escaped = false;
            for (i, &byte) in bytes.iter().enumerate().skip(1) {
                if byte == *quote && !escaped {
                    return i + 1;
                }
                escaped = byte == b'\\' && !escaped;
            }
            text.len()
        }
        _ => text
            .find(|c: char| "&;,}]\r\n".contains(c))
            .unwrap_or(text.len()),
    }
}

/// The end of a value quoted as `\"...\"`, as in JSON inside a string. Its
/// closing quote follows one backslash, and a quote inside it three, so a
/// run of 1, 5, 9... backslashes ends it.
fn escaped_end(bytes: &[u8], quote: u8) -> usize {
    let mut run = 0;
    for (i, &byte) in bytes.iter().enumerate().skip(2) {
        if byte == quote && run % 4 == 1 {
            return i + 1;
        }
        run = if byte == b'\\' { run + 1 } else { 0 };
    }
    bytes.len()
}

/// Scrubs `text` as `policy` says. False when the meter refused to pay.
pub(super) fn scrub(policy: &Policy, text: &mut String, meter: &mut Meter<'_>) -> bool {
    let all = |_: &str, _: usize, _: usize| true;
    field(policy, text, meter)
        && (!policy.email || filtered(&EMAIL, text, super::EMAIL, meter, all))
        && (!policy.ip || filtered(&IP, text, IP_MARKER, meter, ip_at))
        && (!policy.cards || filtered(&CARD, text, super::CARD, meter, card_at))
}

/// Scrubs a structural field: sensitive assignments and credentials, but
/// not the e-mail, IP address and card patterns that a release, an id or a
/// slug can look like. False when the meter refused to pay.
pub(super) fn field(policy: &Policy, text: &mut String, meter: &mut Meter<'_>) -> bool {
    let all = |_: &str, _: usize, _: usize| true;
    let keys = policy.keys || !policy.extra_keys.is_empty();
    (!keys || assignments(policy, text, meter))
        && (!policy.credentials
            || (filtered(&PRIVATE, text, PRIVATE_MARKER, meter, all)
                && filtered(&CREDENTIAL, text, CREDENTIAL_MARKER, meter, all)))
}

fn ip_at(text: &str, start: usize, end: usize) -> bool {
    standalone(text, start, end) && is_ip(&text[start..end])
}

fn card_at(text: &str, start: usize, end: usize) -> bool {
    has_card(&text[start..end])
}

/// The marker an object key becomes when it holds a value `policy` would
/// remove from text, as when a map is keyed by e-mail address.
pub(super) fn key_marker(policy: &Policy, key: &str) -> Option<&'static str> {
    if policy.credentials && key.len() >= 5 && (PRIVATE.is_match(key) || CREDENTIAL.is_match(key)) {
        return Some(CREDENTIAL_MARKER);
    }
    if policy.email && key.contains('@') && EMAIL.is_match(key) {
        return Some(super::EMAIL);
    }
    if policy.ip
        && key.contains(['.', ':'])
        && IP
            .find_iter(key)
            .any(|found| ip_at(key, found.start(), found.end()))
    {
        return Some(IP_MARKER);
    }
    if policy.cards
        && key.bytes().filter(u8::is_ascii_digit).count() >= 13
        && CARD.find_iter(key).any(|found| has_card(found.as_str()))
    {
        return Some(super::CARD);
    }
    None
}

/// Nothing that continues a name, a version or an address on either side.
/// Letters are ASCII, so an address right after `é` is still found.
/// A dot after the candidate may end a sentence.
fn standalone(text: &str, start: usize, end: usize) -> bool {
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let before = text[..start].chars().next_back();
    let mut after = text[end..].chars();
    let free_after = match after.next() {
        None => true,
        Some('.') => !after.next().is_some_and(word),
        Some(c) => !word(c) && c != ':',
    };
    free_after && !before.is_some_and(|c| word(c) || c == '.' || c == ':')
}

/// An address, or an IPv4 address with a port as in `10.0.0.5:5432`.
fn is_ip(candidate: &str) -> bool {
    let public = |ip: IpAddr| !ip.is_loopback() && !ip.is_unspecified();
    if let Ok(ip) = candidate.parse::<IpAddr>() {
        return public(ip);
    }
    candidate.rsplit_once(':').is_some_and(|(host, port)| {
        !port.is_empty()
            && port.bytes().all(|b| b.is_ascii_digit())
            && host.parse::<Ipv4Addr>().is_ok_and(|ip| public(ip.into()))
    })
}

/// Replaces each match of `pattern` that `keep` accepts with `marker`.
/// False when the meter refused to pay.
fn filtered(
    pattern: &Regex,
    text: &mut String,
    marker: &str,
    meter: &mut Meter<'_>,
    keep: impl Fn(&str, usize, usize) -> bool,
) -> bool {
    let mut rewrite = Rewrite::new(text);
    for found in pattern.find_iter(text) {
        if keep(text, found.start(), found.end())
            && !rewrite.replace(found.start(), found.end(), marker, meter)
        {
            rewrite.abandon(meter);
            return false;
        }
    }
    rewrite.finish(meter).apply(text, meter)
}

pub(super) fn has_card(text: &str) -> bool {
    let mut digits = [0; 19];
    let mut count = 0;
    for byte in text.bytes().filter(u8::is_ascii_digit) {
        if count == digits.len() {
            return false;
        }
        digits[count] = byte - b'0';
        count += 1;
    }
    is_card(&digits[..count])
}

/// Whether an integer, written out, is a card number.
pub(super) fn is_card_number(number: &serde_json::Number) -> bool {
    let Some(mut n) = number
        .as_u64()
        .or_else(|| number.as_i64().map(i64::unsigned_abs))
    else {
        return false;
    };
    let mut digits = [0; 20];
    let mut count = 0;
    while n > 0 || count == 0 {
        digits[count] = u8::try_from(n % 10).unwrap_or_default();
        n /= 10;
        count += 1;
    }
    digits[..count].reverse();
    is_card(&digits[..count])
}

fn is_card(digits: &[u8]) -> bool {
    (13..=19).contains(&digits.len()) && (2..=6).contains(&digits[0]) && luhn(digits)
}

fn luhn(digits: &[u8]) -> bool {
    let sum: u32 = digits
        .iter()
        .rev()
        .enumerate()
        .map(|(i, &d)| {
            let d = u32::from(d) * if i % 2 == 1 { 2 } else { 1 };
            if d > 9 { d - 9 } else { d }
        })
        .sum();
    digits.iter().any(|&d| d != 0) && sum.is_multiple_of(10)
}
