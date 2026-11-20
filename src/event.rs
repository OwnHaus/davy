use crate::time;
use anyhow::{Result, bail};
use serde::Serialize;
use serde_json::Value;

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
    fn parse(s: &str) -> Self {
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
}
#[derive(Debug)]
pub struct Exception {
    pub ty: String,
    pub value: Option<String>,
    pub module: Option<String>,
    pub frames: Vec<Frame>,
}
#[derive(Debug)]
pub struct Frame {
    pub filename: Option<String>,
    pub abs_path: Option<String>,
    pub module: Option<String>,
    pub function: Option<String>,
    pub lineno: Option<u32>,
    pub colno: Option<u32>,
    pub in_app: Option<bool>,
}
impl Exception {
    fn parse(v: &Value) -> Self {
        Self {
            ty: string(&v["type"]).unwrap_or_default(),
            value: string(&v["value"]),
            module: string(&v["module"]),
            frames: v["stacktrace"]["frames"]
                .as_array()
                .map(|frames| frames.iter().map(Frame::parse).collect())
                .unwrap_or_default(),
        }
    }
}
impl Frame {
    fn parse(v: &Value) -> Self {
        let number = |k: &str| v[k].as_u64().and_then(|n| u32::try_from(n).ok());
        Self {
            filename: string(&v["filename"]),
            abs_path: string(&v["abs_path"]),
            module: string(&v["module"]),
            function: string(&v["function"]),
            lineno: number("lineno"),
            colno: number("colno"),
            in_app: v["in_app"].as_bool(),
        }
    }
}
fn timestamp(v: Option<&Value>) -> Option<Timestamp> {
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
        let ex = &raw["exception"];
        let exceptions = ex
            .as_array()
            .or_else(|| ex["values"].as_array())
            .map(|values| values.iter().map(Exception::parse).collect())
            .unwrap_or_default();
        let entry = raw
            .get("logentry")
            .or_else(|| raw.get("message").filter(|v| v.is_object()));
        let message = entry
            .and_then(|v| string(&v["formatted"]).or_else(|| string(&v["message"])))
            .or_else(|| string(&raw["message"]));
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
            trace_id: string(&raw["contexts"]["trace"]["trace_id"]),
            raw,
        })
    }
}
