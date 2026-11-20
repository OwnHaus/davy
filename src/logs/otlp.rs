use super::{LogLevel, LogRecord, LogSource, binary, hex_bytes, message, mime};
use opentelemetry_proto::tonic::{
    collector::logs::v1::ExportLogsServiceRequest,
    common::v1::{AnyValue, KeyValue, any_value},
};
use prost::Message;
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
pub struct Status {
    #[prost(int32, tag = "1")]
    pub code: i32,
    #[prost(string, tag = "2")]
    pub message: String,
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
    let request = match encoding {
        Encoding::Protobuf => ExportLogsServiceRequest::decode(body).map_err(|e| e.to_string())?,
        Encoding::Json => serde_json::from_slice(body).map_err(|e| e.to_string())?,
    };
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
        for scope_logs in resource_logs.scope_logs {
            let scope = scope_logs.scope.map(|s| s.name).filter(|n| !n.is_empty());
            for r in scope_logs.log_records {
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
                records.push(LogRecord {
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
                });
            }
        }
    }
    Ok(records)
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
