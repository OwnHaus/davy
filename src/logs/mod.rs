pub mod logtail;
pub mod otlp;
pub mod sentry;
pub mod store;

use crate::time;
use serde::Serialize;
use serde_json::{Map, Value};

pub const ORIGINAL_TIMESTAMP: &str = "davy.original_timestamp";
pub const TRUNCATED: &str = "davy.truncated";
const MAX_MESSAGE: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct LogRecord {
    pub project_id: i64,
    pub timestamp: i64,
    pub observed_at: i64,
    pub source: LogSource,
    pub level: LogLevel,
    pub message: String,
    pub service: Option<String>,
    pub trace_id: Option<String>,
    pub span_id: Option<String>,
    pub attributes: Map<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LogSource {
    Logtail,
    Otlp,
    Sentry,
}

impl LogSource {
    pub fn code(self) -> u64 {
        match self {
            Self::Logtail => 1,
            Self::Otlp => 2,
            Self::Sentry => 3,
        }
    }
    pub fn from_code(code: u64) -> Self {
        match code {
            2 => Self::Otlp,
            3 => Self::Sentry,
            _ => Self::Logtail,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Trace = 1,
    Debug = 2,
    #[default]
    Info = 3,
    Warn = 4,
    Error = 5,
    Fatal = 6,
}

/// Where the log indexes of the data directory `data` live.
pub fn dir(data: &std::path::Path) -> std::path::PathBuf {
    data.join("logs")
}

impl LogLevel {
    /// [`LogLevel::parse`] for a level a person or an agent typed.
    pub fn named(s: &str) -> Result<Self, String> {
        Self::parse(s).ok_or_else(|| format!("unknown level {s:?}"))
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "trace" => Self::Trace,
            "debug" => Self::Debug,
            "info" | "information" | "notice" | "log" => Self::Info,
            "warn" | "warning" => Self::Warn,
            "error" | "err" => Self::Error,
            "fatal" | "critical" | "crit" | "alert" | "emerg" | "emergency" | "panic" => {
                Self::Fatal
            }
            _ => return None,
        })
    }
    pub fn from_pino(n: f64) -> Self {
        match n {
            n if n <= 10.0 => Self::Trace,
            n if n <= 20.0 => Self::Debug,
            n if n <= 30.0 => Self::Info,
            n if n <= 40.0 => Self::Warn,
            n if n <= 50.0 => Self::Error,
            _ => Self::Fatal,
        }
    }
    pub fn from_severity(number: i32, text: &str) -> Self {
        match number {
            1..=4 => Self::Trace,
            5..=8 => Self::Debug,
            9..=12 => Self::Info,
            13..=16 => Self::Warn,
            17..=20 => Self::Error,
            21..=24 => Self::Fatal,
            _ => Self::parse(text).unwrap_or_default(),
        }
    }
    pub fn from_value(v: Option<&Value>) -> Self {
        match v {
            Some(Value::Number(n)) => n.as_f64().map(Self::from_pino).unwrap_or_default(),
            Some(Value::String(s)) => Self::parse(s).unwrap_or_default(),
            _ => Self::Info,
        }
    }
    pub fn from_code(code: u64) -> Self {
        match code {
            1 => Self::Trace,
            2 => Self::Debug,
            4 => Self::Warn,
            5 => Self::Error,
            6 => Self::Fatal,
            _ => Self::Info,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Trace => "trace",
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
            Self::Fatal => "fatal",
        }
    }
}

pub fn timestamp(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => match n.as_i64() {
            Some(i) if i < 0 => None,
            Some(i) if i >= 100_000_000_000_000_000 => Some(i),
            _ => n.as_f64().and_then(scale),
        },
        Value::String(s) => time::parse(s).and_then(time::nanos),
        _ => None,
    }
}

fn scale(n: f64) -> Option<i64> {
    if !n.is_finite() || n < 0.0 {
        return None;
    }
    let (micros, factor) = match n {
        n if n < 1e11 => (n * 1e6, 1000),
        n if n < 1e14 => (n * 1e3, 1000),
        n if n < 1e17 => (n, 1000),
        n => (n, 1),
    };
    time::whole(micros.round())?.checked_mul(factor)
}

fn settle(raw: Option<Value>, observed: i64, attributes: &mut Map<String, Value>) -> i64 {
    if let Some(ts) = raw.as_ref().and_then(timestamp) {
        return ts;
    }
    if let Some(raw) = raw.filter(|v| !v.is_null()) {
        attributes.insert(ORIGINAL_TIMESTAMP.into(), raw);
    }
    observed
}

pub fn hex_id(s: &str, len: usize) -> Option<String> {
    let s = s.to_ascii_lowercase();
    (s.len() == len && s.bytes().all(|b| b.is_ascii_hexdigit()) && s.bytes().any(|b| b != b'0'))
        .then_some(s)
}

pub fn hex_bytes(b: &[u8], len: usize) -> Option<String> {
    hex_id(&crate::text::hex(b), len)
}

/// The text of a record's message, cut to 64 KiB at a character boundary.
/// A cut sets the attribute [`TRUNCATED`].
fn message(v: Option<Value>, attributes: &mut Map<String, Value>) -> String {
    let mut text = match v {
        Some(Value::String(s)) => s,
        None | Some(Value::Null) => String::new(),
        Some(other) => other.to_string(),
    };
    if text.len() > MAX_MESSAGE {
        text.truncate(text.floor_char_boundary(MAX_MESSAGE));
        attributes.insert(TRUNCATED.into(), Value::Bool(true));
    }
    text
}

#[cfg(test)]
mod tests;
