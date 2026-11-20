use super::{LogLevel, LogRecord, LogSource, binary, hex_bytes, message, mime};
use crate::heap::{self, text};
use opentelemetry_proto::tonic::{
    collector::logs::v1::ExportLogsServiceRequest,
    common::v1::{AnyValue, EntityRef, KeyValue, any_value},
    logs::v1::{self, ResourceLogs, ScopeLogs},
};
use prost::{
    Message,
    encoding::{DecodeContext, WireType, decode_key, decode_varint, skip_field},
};
use serde_json::{Map, Value};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Encoding {
    Protobuf,
    Json,
}

impl Encoding {
    pub fn from_content_type(content_type: &str) -> Option<Self> {
        match mime(content_type).as_str() {
            "application/x-protobuf" | "application/protobuf" => Some(Self::Protobuf),
            "application/json" => Some(Self::Json),
            _ => None,
        }
    }
    pub fn content_type(self) -> &'static str {
        match self {
            Self::Protobuf => "application/x-protobuf",
            Self::Json => "application/json",
        }
    }
}

#[derive(Clone, PartialEq, Message)]
struct Status {
    #[prost(int32, tag = "1")]
    code: i32,
    #[prost(string, tag = "2")]
    message: String,
}

pub fn status(encoding: Encoding, message: &str) -> Vec<u8> {
    let s = Status {
        code: 3,
        message: message.into(),
    };
    match encoding {
        Encoding::Protobuf => s.encode_to_vec(),
        Encoding::Json => {
            serde_json::to_vec(&serde_json::json!({"code": s.code, "message": s.message}))
                .unwrap_or_default()
        }
    }
}

pub fn success(encoding: Encoding) -> Vec<u8> {
    match encoding {
        Encoding::Protobuf => Vec::new(),
        Encoding::Json => b"{}".to_vec(),
    }
}

pub fn parse(
    body: &[u8],
    encoding: Encoding,
    project_id: i64,
    observed_at: i64,
) -> Result<Vec<LogRecord>, String> {
    let request = decode(body, encoding)?;
    let free = |_| Ok::<(), std::convert::Infallible>(());
    match convert(request, project_id, observed_at, free) {
        Ok(records) => Ok(records),
    }
}

pub fn decode(body: &[u8], encoding: Encoding) -> Result<ExportLogsServiceRequest, String> {
    match encoding {
        Encoding::Protobuf => ExportLogsServiceRequest::decode(body).map_err(|e| e.to_string()),
        // serde_json skips the text of a field it does not read without
        // checking it, and JSON must be UTF-8 throughout.
        Encoding::Json => std::str::from_utf8(body)
            .map_err(|e| e.to_string())
            .and_then(|text| serde_json::from_str(text).map_err(|e| e.to_string())),
    }
}

/// The records of a request, each paid with `pay` as it is built. A record
/// holds its resource's attributes, so a small request can describe many
/// copies of a large resource, and this is what stops it.
pub fn convert<E>(
    request: ExportLogsServiceRequest,
    project_id: i64,
    observed_at: i64,
    mut pay: impl FnMut(usize) -> Result<(), E>,
) -> Result<Vec<LogRecord>, E> {
    let mut records = Vec::new();
    for resource_logs in request.resource_logs {
        let mut service = None;
        let mut resource = Map::new();
        for kv in resource_logs
            .resource
            .map(|r| r.attributes)
            .unwrap_or_default()
        {
            let (key, value) = pair(kv);
            match (key.as_str(), value) {
                ("service.name", Value::String(s)) => service = Some(s),
                (_, value) => {
                    resource.insert(format!("resource.{key}"), value);
                }
            }
        }
        let copy = heap::of_map(&resource);
        for scope_logs in resource_logs.scope_logs {
            let scope = scope_logs.scope.map(|s| s.name).filter(|n| !n.is_empty());
            for r in scope_logs.log_records {
                pay(copy)?;
                let mut attributes = resource.clone();
                if let Some(scope) = &scope {
                    attributes.insert("otel.scope.name".into(), scope.clone().into());
                }
                if !r.event_name.is_empty() {
                    attributes.insert("otel.event_name".into(), r.event_name.into());
                }
                attributes.extend(r.attributes.into_iter().map(pair));
                let timestamp = [r.time_unix_nano, r.observed_time_unix_nano]
                    .into_iter()
                    .find(|t| *t > 0)
                    .and_then(|t| i64::try_from(t).ok())
                    .unwrap_or(observed_at);
                let message = message(r.body.map(any), &mut attributes);
                let record = LogRecord {
                    project_id,
                    timestamp,
                    observed_at,
                    source: LogSource::Otlp,
                    level: LogLevel::from_severity(r.severity_number, &r.severity_text),
                    message,
                    service: service.clone(),
                    trace_id: hex_bytes(&r.trace_id, 32),
                    span_id: hex_bytes(&r.span_id, 16),
                    attributes,
                };
                // The copy of the resource is in the record's attributes,
                // and was paid before it was made.
                pay(heap::of_record(&record).saturating_sub(copy))?;
                records.push(record);
            }
        }
    }
    Ok(records)
}

/// The heap [`decode`] takes, found without decoding. Errors where a
/// protobuf body does not hold the messages `decode` reads.
pub fn cost(body: &[u8], encoding: Encoding) -> Result<usize, String> {
    match encoding {
        // Each JSON object becomes a message, which takes no more than the
        // largest of them, held as a growing list holds it.
        Encoding::Json => heap::json(body, 3 * LARGEST).map_err(|e| e.to_string()),
        Encoding::Protobuf => walk(body, &REQUEST, 0),
    }
}

/// What a field of an OTLP log message holds on the heap once decoded.
/// Fields that are not listed hold nothing there.
enum Field {
    Text,
    Texts,
    /// A message in its parent, which takes heap only for its own fields.
    One(&'static [(u32, Field)]),
    /// A message in a list, which takes its size in the list as well.
    Many(usize, &'static [(u32, Field)]),
}

const LARGEST: usize = {
    let sizes = [
        size_of::<ResourceLogs>(),
        size_of::<ScopeLogs>(),
        size_of::<v1::LogRecord>(),
        size_of::<KeyValue>(),
        size_of::<AnyValue>(),
        size_of::<EntityRef>(),
    ];
    let mut largest = 0;
    let mut i = 0;
    while i < sizes.len() {
        if sizes[i] > largest {
            largest = sizes[i];
        }
        i += 1;
    }
    largest
};

static REQUEST: [(u32, Field); 1] = [(1, Field::Many(size_of::<ResourceLogs>(), &RESOURCE_LOGS))];
static RESOURCE_LOGS: [(u32, Field); 3] = [
    (1, Field::One(&RESOURCE)),
    (2, Field::Many(size_of::<ScopeLogs>(), &SCOPE_LOGS)),
    (3, Field::Text),
];
static RESOURCE: [(u32, Field); 2] = [
    (1, Field::Many(size_of::<KeyValue>(), &KEY_VALUE)),
    (3, Field::Many(size_of::<EntityRef>(), &ENTITY_REF)),
];
static ENTITY_REF: [(u32, Field); 4] = [
    (1, Field::Text),
    (2, Field::Text),
    (3, Field::Texts),
    (4, Field::Texts),
];
static SCOPE_LOGS: [(u32, Field); 3] = [
    (1, Field::One(&SCOPE)),
    (2, Field::Many(size_of::<v1::LogRecord>(), &LOG_RECORD)),
    (3, Field::Text),
];
static SCOPE: [(u32, Field); 3] = [
    (1, Field::Text),
    (2, Field::Text),
    (3, Field::Many(size_of::<KeyValue>(), &KEY_VALUE)),
];
static LOG_RECORD: [(u32, Field); 6] = [
    (3, Field::Text),
    (5, Field::One(&ANY_VALUE)),
    (6, Field::Many(size_of::<KeyValue>(), &KEY_VALUE)),
    (9, Field::Text),
    (10, Field::Text),
    (12, Field::Text),
];
static KEY_VALUE: [(u32, Field); 2] = [(1, Field::Text), (2, Field::One(&ANY_VALUE))];
static ANY_VALUE: [(u32, Field); 4] = [
    (1, Field::Text),
    (5, Field::One(&ARRAY_VALUE)),
    (6, Field::One(&KEY_VALUE_LIST)),
    (7, Field::Text),
];
static ARRAY_VALUE: [(u32, Field); 1] = [(1, Field::Many(size_of::<AnyValue>(), &ANY_VALUE))];
static KEY_VALUE_LIST: [(u32, Field); 1] = [(1, Field::Many(size_of::<KeyValue>(), &KEY_VALUE))];

/// prost refuses messages nested deeper.
const MAX_DEPTH: usize = 100;

/// The heap the fields in `bytes` take once decoded as a message with
/// `fields`. A list takes its first room of four entries once, and three
/// times its entry size for each entry, as [`heap::VALUE`] explains.
fn walk(mut bytes: &[u8], fields: &[(u32, Field)], depth: usize) -> Result<usize, String> {
    if depth > MAX_DEPTH {
        return Err("message nested too deep".into());
    }
    let mut total = 0;
    let mut lists = 0u32;
    while !bytes.is_empty() {
        let (tag, wire) = decode_key(&mut bytes).map_err(|e| e.to_string())?;
        let skip = match wire {
            WireType::Varint => {
                decode_varint(&mut bytes).map_err(|e| e.to_string())?;
                0
            }
            WireType::SixtyFourBit => 8,
            WireType::ThirtyTwoBit => 4,
            WireType::LengthDelimited => {
                usize::try_from(decode_varint(&mut bytes).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?
            }
            // No OTLP field is a group, so a group is unknown and skipped,
            // as prost skips it, without allocating.
            WireType::StartGroup => {
                skip_field(wire, tag, &mut bytes, DecodeContext::default())
                    .map_err(|e| e.to_string())?;
                continue;
            }
            WireType::EndGroup => return Err("end of a group that never started".into()),
        };
        let (inner, rest) = bytes.split_at_checked(skip).ok_or("message cut short")?;
        bytes = rest;
        let Some(i) = fields.iter().position(|(t, _)| *t == tag) else {
            continue;
        };
        if wire != WireType::LengthDelimited {
            continue;
        }
        let mut first_room = |size: usize| {
            let first = lists & (1 << i) == 0;
            lists |= 1 << i;
            if first { 4 * size } else { 0 }
        };
        total += match &fields[i].1 {
            Field::Text => text(inner.len()),
            Field::Texts => {
                let size = size_of::<String>();
                first_room(size) + 3 * size + text(inner.len())
            }
            Field::One(sub) => walk(inner, sub, depth + 1)?,
            Field::Many(size, sub) => first_room(*size) + 3 * size + walk(inner, sub, depth + 1)?,
        };
    }
    Ok(total)
}

fn pair(kv: KeyValue) -> (String, Value) {
    (kv.key, kv.value.map_or(Value::Null, any))
}

fn any(v: AnyValue) -> Value {
    use any_value::Value as V;
    match v.value {
        Some(V::StringValue(s)) => s.into(),
        Some(V::BoolValue(b)) => b.into(),
        Some(V::IntValue(i)) => i.into(),
        Some(V::DoubleValue(f)) => f.into(),
        Some(V::ArrayValue(a)) => a.values.into_iter().map(any).collect(),
        Some(V::KvlistValue(l)) => Value::Object(l.values.into_iter().map(pair).collect()),
        Some(V::BytesValue(b)) => binary(b),
        _ => Value::Null,
    }
}
