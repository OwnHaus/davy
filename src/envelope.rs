use crate::event::EventId;
use anyhow::{Result, bail};
use axum::body::Bytes;
use serde_json::Value;
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
    Other(String),
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
            _ => Self::Other(s.into()),
        }
    }
}
#[derive(Debug, PartialEq, Eq)]
pub enum DataCategory {
    Transaction,
    Span,
    Session,
    Internal,
    Attachment,
    Replay,
    Profile,
    Monitor,
    Unknown,
}
#[derive(Debug, PartialEq, Eq)]
pub enum Disposition {
    StoreError,
    StoreLogs,
    Drop(DataCategory),
}
pub fn disposition(kind: &ItemKind) -> Disposition {
    use DataCategory as C;
    Disposition::Drop(match kind {
        ItemKind::Event => return Disposition::StoreError,
        ItemKind::Log => return Disposition::StoreLogs,
        ItemKind::Transaction => C::Transaction,
        ItemKind::Span => C::Span,
        ItemKind::Session | ItemKind::Sessions => C::Session,
        ItemKind::ClientReport => C::Internal,
        ItemKind::Attachment => C::Attachment,
        ItemKind::ReplayEvent | ItemKind::ReplayRecording => C::Replay,
        ItemKind::Profile | ItemKind::ProfileChunk => C::Profile,
        ItemKind::CheckIn => C::Monitor,
        ItemKind::Other(_) => C::Unknown,
    })
}
pub struct EnvelopeHeader {
    pub event_id: Option<EventId>,
    pub dsn: Option<String>,
    pub sent_at: Option<String>,
}
pub struct Item {
    pub kind: ItemKind,
    pub payload: Bytes,
}
pub struct Envelope {
    pub header: EnvelopeHeader,
    pub items: Vec<Item>,
}
fn line(input: &mut &[u8]) -> Result<Value> {
    let n = input
        .iter()
        .position(|b| *b == b'\n')
        .unwrap_or(input.len());
    let v = serde_json::from_slice(&input[..n])?;
    *input = &input[(n + 1).min(input.len())..];
    Ok(v)
}
impl EnvelopeHeader {
    /// Parses the first line of an envelope. Bytes after the first newline are ignored.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        Self::take(&mut &bytes[..])
    }

    fn take(input: &mut &[u8]) -> Result<Self> {
        let h = line(input)?;
        if !h.is_object() {
            bail!("invalid envelope header");
        }
        Ok(Self {
            event_id: h
                .get("event_id")
                .map(|v| EventId::parse(v.as_str().unwrap_or("")))
                .transpose()?,
            dsn: h
                .get("dsn")
                .map(|v| {
                    v.as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| anyhow::anyhow!("invalid dsn"))
                })
                .transpose()?,
            sent_at: h
                .get("sent_at")
                .map(|v| {
                    v.as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| anyhow::anyhow!("invalid sent_at"))
                })
                .transpose()?,
        })
    }
}
impl Envelope {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
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
            let h = line(&mut input)?;
            let kind = ItemKind::parse(
                h["type"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("missing item type"))?,
            );
            let n = match h.get("length") {
                Some(v) => usize::try_from(
                    v.as_u64()
                        .ok_or_else(|| anyhow::anyhow!("invalid length"))?,
                )?,
                None => input
                    .iter()
                    .position(|b| *b == b'\n')
                    .unwrap_or(input.len()),
            };
            if n > input.len() {
                bail!("truncated item");
            }
            items.push(Item {
                kind,
                payload: Bytes::copy_from_slice(&input[..n]),
            });
            input = &input[n..];
            if input.first() == Some(&b'\n') {
                input = &input[1..];
            }
        }
        Ok(Self { header, items })
    }
}
