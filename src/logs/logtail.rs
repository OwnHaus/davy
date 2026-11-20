use super::{LogLevel, LogRecord, LogSource, hex_id, settle, text};
use serde_json::{Map, Value};

const MSGPACK_TIMESTAMP: i8 = -1;

pub fn parse(
    body: &[u8],
    content_type: &str,
    project_id: i64,
    observed_at: i64,
) -> Result<Vec<LogRecord>, String> {
    let mime = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let values = match mime.as_str() {
        "application/msgpack" | "application/x-msgpack" => vec![msgpack(body)?],
        "application/x-ndjson" | "application/ndjson" => ndjson(body)?,
        "application/json" => json(body)?,
        _ => match body.iter().find(|b| !b.is_ascii_whitespace()) {
            Some(b'[' | b'{') => json(body)?,
            _ => vec![msgpack(body)?],
        },
    };
    let mut records = Vec::new();
    for value in values {
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

fn json(body: &[u8]) -> Result<Vec<Value>, String> {
    match serde_json::from_slice(body) {
        Ok(v) => Ok(vec![v]),
        Err(e) => ndjson(body).map_err(|line_err| format!("{e} (as ndjson: {line_err})")),
    }
}

fn ndjson(body: &[u8]) -> Result<Vec<Value>, String> {
    body.split(|b| *b == b'\n')
        .filter(|l| !l.iter().all(u8::is_ascii_whitespace))
        .map(|l| serde_json::from_slice(l).map_err(|e| e.to_string()))
        .collect()
}

fn msgpack(mut body: &[u8]) -> Result<Value, String> {
    let v = rmpv::decode::read_value(&mut body).map_err(|e| e.to_string())?;
    Ok(to_json(v))
}

fn to_json(v: rmpv::Value) -> Value {
    use rmpv::Value as M;
    match v {
        M::Boolean(b) => b.into(),
        M::Integer(i) => i
            .as_i64()
            .map(Value::from)
            .or_else(|| i.as_u64().map(Value::from))
            .unwrap_or(Value::Null),
        M::F32(f) => Value::from(f64::from(f)),
        M::F64(f) => Value::from(f),
        M::String(s) => s.into_str().map_or(Value::Null, Value::from),
        M::Binary(b) => {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD.encode(b).into()
        }
        M::Array(a) => a.into_iter().map(to_json).collect(),
        M::Map(m) => Value::Object(
            m.into_iter()
                .map(|(k, v)| {
                    let k = match k {
                        M::String(s) => s.into_str().unwrap_or_default(),
                        other => other.to_string(),
                    };
                    (k, to_json(v))
                })
                .collect(),
        ),
        M::Ext(MSGPACK_TIMESTAMP, b) => msgpack_timestamp(&b).map_or(Value::Null, Value::from),
        M::Nil | M::Ext(..) => Value::Null,
    }
}

pub fn msgpack_timestamp(b: &[u8]) -> Option<i64> {
    let (secs, nanos) = match b.len() {
        4 => (i64::from(u32::from_be_bytes(b.try_into().ok()?)), 0),
        8 => {
            let v = u64::from_be_bytes(b.try_into().ok()?);
            (
                i64::try_from(v & 0x3_ffff_ffff).ok()?,
                i64::try_from(v >> 34).ok()?,
            )
        }
        12 => (
            i64::from_be_bytes(b[4..].try_into().ok()?),
            i64::from(u32::from_be_bytes(b[..4].try_into().ok()?)),
        ),
        _ => return None,
    };
    secs.checked_mul(1_000_000_000)?.checked_add(nanos)
}

fn take_str(m: &mut Map<String, Value>, key: &str) -> Option<String> {
    match m.remove(key) {
        Some(Value::String(s)) => Some(s),
        Some(other) => {
            m.insert(key.into(), other);
            None
        }
        None => None,
    }
}

fn take_id(m: &mut Map<String, Value>, key: &str, len: usize) -> Option<String> {
    let id = m
        .get(key)
        .and_then(Value::as_str)
        .and_then(|s| hex_id(s, len));
    if id.is_some() {
        m.remove(key);
    }
    id
}

fn record(v: Value, project_id: i64, observed_at: i64) -> Option<LogRecord> {
    let Value::Object(mut attributes) = v else {
        return None;
    };
    let dt = attributes.remove("dt");
    let message = attributes.remove("message").map(text).unwrap_or_default();
    let level = LogLevel::from_value(attributes.remove("level").as_ref());
    let service = take_str(&mut attributes, "service");
    let trace_id = take_id(&mut attributes, "trace_id", 32);
    let span_id = take_id(&mut attributes, "span_id", 16);
    let timestamp = settle(dt, observed_at, &mut attributes);
    Some(LogRecord {
        project_id,
        timestamp,
        observed_at,
        source: LogSource::Logtail,
        level,
        message,
        service,
        trace_id,
        span_id,
        attributes,
    })
}
