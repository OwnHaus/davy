use crate::event::EventId;
use anyhow::{Result, anyhow, bail};
use serde::Deserialize;
use std::borrow::Cow;
#[derive(Debug, PartialEq, Eq)]
pub enum ItemKind {
    Event,
    Transaction,
    Span,
    Session,
    Sessions,
    ClientReport,
    Attachment,
    Log,
    ReplayEvent,
    ReplayRecording,
    Profile,
    ProfileChunk,
    CheckIn,
    Feedback,
    UserReport,
    Other,
}
impl ItemKind {
    fn parse(s: &str) -> Self {
        match s {
            "event" => Self::Event,
            "transaction" => Self::Transaction,
            "span" => Self::Span,
            "session" => Self::Session,
            "sessions" => Self::Sessions,
            "client_report" => Self::ClientReport,
            "attachment" => Self::Attachment,
            "log" => Self::Log,
            "replay_event" => Self::ReplayEvent,
            "replay_recording" => Self::ReplayRecording,
            "profile" => Self::Profile,
            "profile_chunk" => Self::ProfileChunk,
            "check_in" => Self::CheckIn,
            "feedback" => Self::Feedback,
            "user_report" => Self::UserReport,
            _ => Self::Other,
        }
    }
}
#[derive(Debug, PartialEq, Eq)]
pub enum Disposition {
    StoreError,
    StoreLogs,
    /// Kept in SQLite through [`crate::items`].
    SdkItem,
    Drop,
}
pub fn disposition(kind: &ItemKind) -> Disposition {
    match kind {
        ItemKind::Event => Disposition::StoreError,
        ItemKind::Log => Disposition::StoreLogs,
        ItemKind::ClientReport
        | ItemKind::Feedback
        | ItemKind::UserReport
        | ItemKind::Session
        | ItemKind::Sessions => Disposition::SdkItem,
        _ => Disposition::Drop,
    }
}
pub struct EnvelopeHeader {
    pub event_id: Option<EventId>,
    pub dsn: Option<String>,
}
pub struct Item<'a> {
    pub kind: ItemKind,
    pub payload: &'a [u8],
}
pub struct Envelope<'a> {
    pub header: EnvelopeHeader,
    pub items: Vec<Item<'a>>,
}
/// More items than any SDK sends in one envelope. Each item costs a little
/// even when it is dropped, so their number has a bound.
pub const MAX_ITEMS: usize = 1000;

/// The header fields Davy reads. Others are skipped without being built, so
/// a long header costs no more memory than a short one.
#[derive(Deserialize)]
struct RawHeader {
    event_id: Option<String>,
    dsn: Option<String>,
}
#[derive(Deserialize)]
struct RawItem<'a> {
    #[serde(rename = "type", borrow)]
    kind: Option<Cow<'a, str>>,
    length: Option<u64>,
}
fn line_end(input: &[u8]) -> usize {
    input
        .iter()
        .position(|b| *b == b'\n')
        .unwrap_or(input.len())
}
/// The JSON object on the first line of `input`, which then starts after it.
fn line<'a, T: Deserialize<'a>>(input: &mut &'a [u8], what: &str) -> Result<T> {
    let (text, rest) = input.split_at(line_end(input));
    if text.trim_ascii_start().first() != Some(&b'{') {
        bail!("invalid {what}");
    }
    let v = serde_json::from_slice(text)?;
    *input = rest.get(1..).unwrap_or_default();
    Ok(v)
}
impl EnvelopeHeader {
    /// Parses the first line of an envelope. Bytes after the first newline are ignored.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        Self::take(&mut &bytes[..])
    }

    fn take(input: &mut &[u8]) -> Result<Self> {
        let h: RawHeader = line(input, "envelope header")?;
        Ok(Self {
            event_id: h.event_id.as_deref().map(EventId::parse).transpose()?,
            dsn: h.dsn,
        })
    }
}
impl<'a> Envelope<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self> {
        let mut input = bytes;
        let header = EnvelopeHeader::take(&mut input)?;
        let mut items = Vec::new();
        loop {
            while let Some((b'\n' | b'\r', rest)) = input.split_first() {
                input = rest;
            }
            if input.is_empty() {
                break;
            }
            if items.len() == MAX_ITEMS {
                bail!("more than {MAX_ITEMS} items");
            }
            let h: RawItem = line(&mut input, "item header")?;
            let kind = ItemKind::parse(&h.kind.ok_or_else(|| anyhow!("missing item type"))?);
            let n = match h.length {
                Some(n) => usize::try_from(n)?,
                None => line_end(input),
            };
            let Some((payload, rest)) = input.split_at_checked(n) else {
                bail!("truncated item");
            };
            items.push(Item { kind, payload });
            input = rest.strip_prefix(b"\n").unwrap_or(rest);
        }
        Ok(Self { header, items })
    }
}
