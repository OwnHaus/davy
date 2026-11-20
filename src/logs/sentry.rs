use super::{LogLevel, LogRecord, LogSource, hex_id, message, settle};
use serde_json::{Map, Value};

pub fn parse(payload: &[u8], project_id: i64, observed_at: i64) -> Result<Vec<LogRecord>, String> {
    let v: Value = serde_json::from_slice(payload).map_err(|e| e.to_string())?;
    let items = match v {
        Value::Object(mut m) => match m.remove("items") {
            Some(Value::Array(items)) => items,
            _ => return Err("log item without an items array".into()),
        },
        _ => return Err("log item is not an object".into()),
    };
    Ok(items
        .into_iter()
        .filter_map(|v| record(v, project_id, observed_at))
        .collect())
}

fn record(v: Value, project_id: i64, observed_at: i64) -> Option<LogRecord> {
    let Value::Object(mut m) = v else {
        return None;
    };
    let mut attributes: Map<String, Value> = match m.remove("attributes") {
        Some(Value::Object(a)) => a
            .into_iter()
            .map(|(k, v)| match v {
                Value::Object(mut typed) if typed.contains_key("value") => {
                    (k, typed.remove("value").unwrap_or(Value::Null))
                }
                other => (k, other),
            })
            .collect(),
        _ => Map::new(),
    };
    let severity = m
        .get("severity_number")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let level_text = m.get("level").and_then(Value::as_str).unwrap_or("");
    let level = match LogLevel::parse(level_text) {
        Some(l) => l,
        None => LogLevel::from_severity(i32::try_from(severity).unwrap_or(0), level_text),
    };
    let id = |m: &Map<String, Value>, key: &str, len| {
        m.get(key)
            .and_then(Value::as_str)
            .and_then(|s| hex_id(s, len))
    };
    let trace_id = id(&m, "trace_id", 32);
    let span_id = id(&m, "span_id", 16);
    let service = attributes
        .get("service.name")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let timestamp = settle(m.remove("timestamp"), observed_at, &mut attributes);
    let message = message(m.remove("body"), &mut attributes);
    Some(LogRecord {
        project_id,
        timestamp,
        observed_at,
        source: LogSource::Sentry,
        level,
        message,
        service,
        trace_id,
        span_id,
        attributes,
    })
}
