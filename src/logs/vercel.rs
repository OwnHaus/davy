//! Vercel Log Drains: the JSON or NDJSON body of a custom drain endpoint.
//! Field names follow Vercel's log drain schema
//! (<https://vercel.com/docs/drains/reference/logs>).

use super::logtail::{json, take_id, take_str};
use super::{LogLevel, LogRecord, LogSource, message, settle};
use crate::heap::{self, RECORD};
use serde_json::{Map, Value};

/// The keys a drain may write a trace id under, newest name first. Older
/// drains send `trace.id` and `span.id` next to `traceId` and `spanId`.
const TRACE: [&str; 2] = ["traceId", "trace.id"];
const SPAN: [&str; 2] = ["spanId", "span.id"];

/// The heap [`parse`] takes, found without parsing: the tree of the body,
/// and as much again for the records built from it. Errors where `parse`
/// would.
pub fn cost(body: &[u8]) -> Result<usize, String> {
    let tree = heap::json(body, RECORD).or_else(|e| {
        heap::ndjson(body, RECORD).map_err(|line| format!("{e} (as ndjson: {line})"))
    })?;
    Ok(2 * tree)
}

/// The records a body holds, read without building them: the elements of
/// an array, 1 for one value, else its lines that are not blank.
pub fn count(body: &[u8]) -> u64 {
    use serde::de::IgnoredAny;
    let first = body.iter().find(|b| !b.is_ascii_whitespace());
    if first == Some(&b'[')
        && let Ok(all) = serde_json::from_slice::<Vec<IgnoredAny>>(body)
    {
        return all.len() as u64;
    }
    if serde_json::from_slice::<IgnoredAny>(body).is_ok() {
        return 1;
    }
    body.split(|b| *b == b'\n')
        .filter(|l| !l.iter().all(u8::is_ascii_whitespace))
        .count() as u64
}

/// A JSON array of records, one record, or one record per line (NDJSON).
/// The content type does not matter: a body that is not one JSON value is
/// read as NDJSON. A record that is not an object is skipped.
pub fn parse(body: &[u8], project_id: i64, observed_at: i64) -> Result<Vec<LogRecord>, String> {
    let mut records = Vec::new();
    for value in json(body)? {
        match value {
            Value::Array(items) => records.extend(
                items
                    .into_iter()
                    .filter_map(|v| record(v, project_id, observed_at)),
            ),
            Value::Object(_) => records.extend(record(value, project_id, observed_at)),
            _ => return Err("expected a log record or an array of records".into()),
        }
    }
    Ok(records)
}

/// The first valid id under `keys`. Every key that holds the same id goes,
/// so a drain that sends both names stores the id once.
fn lift(m: &mut Map<String, Value>, keys: [&str; 2], len: usize) -> Option<String> {
    let id = keys.iter().find_map(|k| take_id(m, k, len))?;
    for key in keys {
        let same = m
            .get(key)
            .and_then(Value::as_str)
            .is_some_and(|s| s.eq_ignore_ascii_case(&id));
        if same {
            m.remove(key);
        }
    }
    Some(id)
}

fn record(v: Value, project_id: i64, observed_at: i64) -> Option<LogRecord> {
    let Value::Object(mut attributes) = v else {
        return None;
    };
    let raw = attributes.remove("timestamp");
    let message = message(attributes.remove("message"), &mut attributes);
    let level = LogLevel::from_value(attributes.remove("level").as_ref());
    // `projectId` stays an attribute: it is what the Vercel API takes.
    let service = take_str(&mut attributes, "projectName")
        .filter(|name| !name.is_empty())
        .or_else(|| {
            attributes
                .get("projectId")
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
    let trace_id = lift(&mut attributes, TRACE, 32);
    let span_id = lift(&mut attributes, SPAN, 16);
    let timestamp = settle(raw, observed_at, &mut attributes);
    Some(LogRecord {
        project_id,
        timestamp,
        observed_at,
        source: LogSource::Vercel,
        level: level.unwrap_or_default(),
        level_unknown: level.is_none(),
        message,
        service,
        trace_id,
        span_id,
        attributes,
    })
}

#[cfg(test)]
#[path = "vercel_tests.rs"]
mod tests;
