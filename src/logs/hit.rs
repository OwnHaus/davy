//! A stored log as a search hit. Hot and cold hits go through the same
//! [`Row`] and [`to_hit`], so a log reads the same from either.

use super::{Fields, LogHit, patterns};
use crate::{
    logs::{LogLevel, LogSource},
    time::rfc3339,
};
use anyhow::Result;
use serde_json::{Map, Value};
use tantivy::{
    TantivyDocument,
    schema::{Field, OwnedValue, Schema, Value as _},
};

/// What a stored log holds: all a hit shows and all a query can match, the
/// project aside.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Row {
    pub(super) ts: i64,
    pub(super) observed: i64,
    /// The code of `LogLevel` as stored.
    pub(super) level: u8,
    /// The code of `LogSource` as stored.
    pub(super) source: u8,
    /// The pattern id, `None` in a day written before patterns.
    pub(super) pattern: Option<u64>,
    pub(super) service: Option<String>,
    pub(super) trace_id: Option<String>,
    pub(super) span_id: Option<String>,
    pub(super) message: String,
    /// The attributes as a JSON object, empty for none.
    pub(super) attributes: String,
}

pub(super) fn hit(schema: &Schema, f: &Fields, doc: &TantivyDocument) -> Result<LogHit> {
    let (project, row) = stored(schema, f, doc)?;
    to_hit(project, &row)
}

/// The project and row of a document of a day index.
pub(super) fn stored(schema: &Schema, f: &Fields, doc: &TantivyDocument) -> Result<(u64, Row)> {
    let first = |field: Field| doc.get_first(field);
    let int = |field: Field| first(field).and_then(|v| v.as_i64()).unwrap_or(0);
    let uint = |field: Field| first(field).and_then(|v| v.as_u64()).unwrap_or(0);
    let code = |field: Field| u8::try_from(uint(field)).unwrap_or(u8::MAX);
    let string = |field: Field| first(field).and_then(|v| v.as_str().map(str::to_owned));
    let attributes = match first(f.attributes).map(|v| OwnedValue::from(v.as_value())) {
        Some(o @ OwnedValue::Object(_)) => match serde_json::to_value(&o)? {
            Value::Object(m) if !m.is_empty() => serde_json::to_string(&m)?,
            _ => String::new(),
        },
        _ => String::new(),
    };
    let row = Row {
        ts: int(f.ts),
        observed: int(f.observed),
        level: code(f.level),
        source: code(f.source),
        // By name: in a day without the field its id names another one.
        pattern: schema
            .get_field("pattern")
            .ok()
            .and_then(|p| first(p)?.as_u64()),
        service: string(f.service),
        trace_id: string(f.trace_id),
        span_id: string(f.span_id),
        message: string(f.message).unwrap_or_default(),
        attributes,
    };
    Ok((uint(f.project), row))
}

/// The hit a row of `project` shows.
pub(super) fn to_hit(project: u64, row: &Row) -> Result<LogHit> {
    Ok(LogHit {
        timestamp: rfc3339(row.ts),
        timestamp_ns: row.ts,
        project_id: project.cast_signed(),
        source: LogSource::from_code(row.source.into()),
        level: LogLevel::from_code(row.level.into()),
        service: row.service.clone(),
        message: row.message.clone(),
        trace_id: row.trace_id.clone(),
        span_id: row.span_id.clone(),
        pattern: row.pattern.map(patterns::show),
        attributes: attributes(&row.attributes)?,
    })
}

/// The attributes a row holds as JSON.
pub(super) fn attributes(json: &str) -> Result<Map<String, Value>> {
    if json.is_empty() {
        return Ok(Map::new());
    }
    Ok(serde_json::from_str(json)?)
}
