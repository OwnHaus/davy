use crate::time;
use anyhow::{Result, bail};
use serde::Serialize;
use serde_json::{Value, json};

pub type Timestamp = i64;
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EventId(pub String);
impl EventId {
    pub fn fresh() -> Self {
        Self(uuid::Uuid::new_v4().simple().to_string())
    }
    pub fn parse(s: &str) -> Result<Self> {
        if s.len() != 32 && s.len() != 36 {
            bail!("invalid event_id");
        }
        Ok(Self(uuid::Uuid::parse_str(s)?.simple().to_string()))
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Level {
    Fatal,
    #[default]
    Error,
    Warning,
    Info,
    Debug,
}
impl Level {
    pub(crate) fn parse(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "fatal" | "critical" => Self::Fatal,
            "warning" | "warn" => Self::Warning,
            "info" | "log" => Self::Info,
            "debug" | "trace" => Self::Debug,
            _ => Self::Error,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fatal => "fatal",
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Info => "info",
            Self::Debug => "debug",
        }
    }
}
#[derive(Debug)]
pub struct ErrorEvent {
    pub event_id: EventId,
    pub timestamp: Timestamp,
    pub received_at: Timestamp,
    pub platform: String,
    pub level: Level,
    pub release: Option<String>,
    pub environment: Option<String>,
    pub message: Option<String>,
    pub message_template: Option<String>,
    pub exceptions: Vec<Exception>,
    pub fingerprint: Option<Vec<String>>,
    pub user_id: Option<String>,
    pub trace_id: Option<String>,
    pub raw: Value,
    /// Whether an uploaded source map moved one of its frames on arrival.
    pub symbolicated: bool,
}
#[derive(Debug)]
pub struct Exception {
    pub ty: String,
    pub value: Option<String>,
    pub frames: Vec<Frame>,
}
#[derive(Debug)]
pub struct Frame {
    pub filename: Option<String>,
    pub abs_path: Option<String>,
    pub module: Option<String>,
    pub function: Option<String>,
    pub in_app: Option<bool>,
}
impl Exception {
    fn parse(v: &Value) -> Self {
        Self {
            ty: string(&v["type"]).unwrap_or_default(),
            value: string(&v["value"]),
            frames: v["stacktrace"]["frames"]
                .as_array()
                .map(|frames| frames.iter().map(Frame::parse).collect())
                .unwrap_or_default(),
        }
    }
}
impl Frame {
    fn parse(v: &Value) -> Self {
        Self {
            filename: string(&v["filename"]),
            abs_path: string(&v["abs_path"]),
            module: string(&v["module"]),
            function: string(&v["function"]),
            in_app: v["in_app"].as_bool(),
        }
    }
}
pub(crate) fn timestamp(v: Option<&Value>) -> Option<Timestamp> {
    let ms = match v? {
        Value::Number(n) => n.as_f64()? * 1000.0,
        Value::String(s) => return time::parse(s).map(jiff::Timestamp::as_millisecond),
        _ => return None,
    };
    if ms < 0.0 {
        return None;
    }
    time::whole(ms)
}
/// The items of a list that SDKs send either bare or as `{"values": [...]}`.
pub(crate) fn values(v: &Value) -> &[Value] {
    v.as_array()
        .or_else(|| v["values"].as_array())
        .map_or(&[], Vec::as_slice)
}
fn string(v: &Value) -> Option<String> {
    v.as_str().map(str::to_owned)
}
impl ErrorEvent {
    pub fn parse(mut raw: Value) -> Result<Self> {
        if !raw.is_object() {
            bail!("event must be an object");
        }
        let event_id = match raw.get("event_id") {
            Some(v) => EventId::parse(
                v.as_str()
                    .ok_or_else(|| anyhow::anyhow!("invalid event_id"))?,
            )?,
            None => EventId::fresh(),
        };
        raw["event_id"] = Value::String(event_id.0.clone());
        let received_at = time::now_ms();
        let timestamp = timestamp(raw.get("timestamp")).unwrap_or(received_at);
        let exceptions = values(&raw["exception"])
            .iter()
            .map(Exception::parse)
            .collect();
        let entry = log_entry(&raw);
        let message = message(&raw);
        Ok(Self {
            event_id,
            timestamp,
            received_at,
            platform: string(&raw["platform"]).unwrap_or_else(|| "other".into()),
            level: raw["level"].as_str().map(Level::parse).unwrap_or_default(),
            release: string(&raw["release"]),
            environment: string(&raw["environment"]),
            message,
            message_template: entry.and_then(|v| string(&v["message"])),
            exceptions,
            fingerprint: raw["fingerprint"].as_array().map(|parts| {
                parts
                    .iter()
                    .filter_map(|v| match v {
                        Value::String(s) => Some(s.clone()),
                        Value::Number(n) => Some(n.to_string()),
                        _ => None,
                    })
                    .collect()
            }),
            user_id: string(&raw["user"]["id"])
                .or_else(|| raw["user"]["id"].as_i64().map(|n| n.to_string())),
            trace_id: raw["contexts"]["trace"]["trace_id"]
                .as_str()
                .and_then(|s| crate::logs::hex_id(s, 32)),
            raw,
            symbolicated: false,
        })
    }
}

/// The structured message of an event, where SDKs put the template.
fn log_entry(raw: &Value) -> Option<&Value> {
    raw.get("logentry")
        .or_else(|| raw.get("message").filter(|v| v.is_object()))
}

/// The message of an event, formatted when the SDK sent a template.
pub(crate) fn message(raw: &Value) -> Option<String> {
    log_entry(raw)
        .and_then(|v| string(&v["formatted"]).or_else(|| string(&v["message"])))
        .or_else(|| string(&raw["message"]))
}

const MAX_FRAMES: usize = 30;
const MAX_BREADCRUMBS: usize = 20;

/// The parts of a stored event that help someone fix the bug, small enough
/// for an agent's context: the innermost 30 frames per exception with source
/// lines only for in-app frames, the last 20 breadcrumbs, and the runtime.
/// Empty fields are left out.
pub fn compact(e: &ErrorEvent) -> Value {
    let raw = &e.raw;
    let exceptions: Vec<Value> = values(&raw["exception"])
        .iter()
        .map(|x| {
            let frames = x["stacktrace"]["frames"].as_array().map_or(&[][..], Vec::as_slice);
            json!({
                "type": x["type"],
                "value": x["value"],
                "mechanism": x["mechanism"]["type"],
                "handled": x["mechanism"]["handled"],
                "frames": frames[frames.len().saturating_sub(MAX_FRAMES)..].iter().map(compact_frame).collect::<Vec<_>>(),
            })
        })
        .collect();
    let crumbs = values(&raw["breadcrumbs"]);
    let breadcrumbs: Vec<Value> = crumbs[crumbs.len().saturating_sub(MAX_BREADCRUMBS)..]
        .iter()
        .map(|b| {
            json!({
                "timestamp": timestamp(b.get("timestamp")).map(time::rfc3339_ms),
                "type": b["type"],
                "category": b["category"],
                "level": b["level"],
                "message": b["message"],
                "data": b["data"],
            })
        })
        .collect();
    let named = |k: &str| json!({"name": raw["contexts"][k]["name"], "version": raw["contexts"][k]["version"]});
    prune(json!({
        "event_id": e.event_id.0,
        "timestamp": time::rfc3339_ms(e.timestamp),
        "level": e.level.as_str(),
        "platform": e.platform,
        "release": e.release,
        "environment": e.environment,
        "message": e.message,
        "trace_id": e.trace_id,
        "exceptions": exceptions,
        "breadcrumbs": breadcrumbs,
        "tags": raw["tags"],
        "user": raw["user"],
        "request": {"method": raw["request"]["method"], "url": raw["request"]["url"]},
        "runtime": named("runtime"),
        "os": named("os"),
        "browser": named("browser"),
    }))
}

fn compact_frame(f: &Value) -> Value {
    let mut out = json!({
        "function": f["function"],
        "module": f["module"],
        "filename": f["filename"],
        "lineno": f["lineno"],
        "colno": f["colno"],
        "in_app": f["in_app"],
    });
    if f["in_app"] == true {
        for k in ["pre_context", "context_line", "post_context"] {
            out[k] = f[k].clone();
        }
    }
    out
}

/// Drops nulls and empty arrays and objects, recursively.
fn prune(v: Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.into_iter()
                .map(|(k, v)| (k, prune(v)))
                .filter(|(_, v)| !is_empty(v))
                .collect(),
        ),
        Value::Array(a) => {
            Value::Array(a.into_iter().map(prune).filter(|v| !is_empty(v)).collect())
        }
        other => other,
    }
}

fn is_empty(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::Array(a) => a.is_empty(),
        Value::Object(m) => m.is_empty(),
        _ => false,
    }
}
