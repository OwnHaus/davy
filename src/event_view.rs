//! What the issue page shows of one stored event. Every list and every
//! string has a limit, and a cut list says how many items it left out, so
//! one huge event cannot make a huge response.

use crate::{
    event::{Level, message, timestamp, values},
    time,
};
use serde::Serialize;
use serde_json::Value;

/// Frames per exception, the innermost kept.
pub const MAX_FRAMES: usize = 256;
/// Breadcrumbs, the newest kept.
pub const MAX_BREADCRUMBS: usize = 200;
/// Tags, fields of the user, of one context or of one breadcrumb.
pub const MAX_PAIRS: usize = 200;
/// Exceptions of a chain, the outermost kept.
pub const MAX_EXCEPTIONS: usize = 32;
/// Contexts such as runtime and os.
pub const MAX_CONTEXTS: usize = 32;
/// Source lines before and after the line of a frame.
pub const MAX_CONTEXT_LINES: usize = 32;
/// Bytes of one string.
pub const MAX_STRING: usize = 64 * 1024;

/// Some items of a list, and how many of the rest it left out.
#[derive(Debug, PartialEq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct Cut<T> {
    pub items: Vec<T>,
    pub more: usize,
}

#[derive(Debug, PartialEq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct EventView {
    pub event_id: String,
    pub timestamp: Option<String>,
    pub level: String,
    pub platform: Option<String>,
    pub release: Option<String>,
    pub environment: Option<String>,
    pub message: Option<String>,
    pub trace_id: Option<String>,
    /// In the order the SDK sent them, the exception that was thrown last
    /// at the end.
    pub exceptions: Cut<ExceptionView>,
    pub breadcrumbs: Cut<Breadcrumb>,
    pub tags: Cut<Pair>,
    pub request: Option<Request>,
    pub user: Cut<Pair>,
    /// Runtime, os and browser first, then the rest by name.
    pub contexts: Cut<Context>,
}

#[derive(Debug, PartialEq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct ExceptionView {
    #[serde(rename = "type")]
    #[cfg_attr(test, ts(rename = "type"))]
    pub ty: Option<String>,
    pub value: Option<String>,
    pub mechanism: Option<String>,
    pub handled: Option<bool>,
    /// The innermost frame, where it was thrown, at the end.
    pub frames: Cut<FrameView>,
}

#[derive(Debug, PartialEq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct FrameView {
    pub function: Option<String>,
    pub module: Option<String>,
    pub filename: Option<String>,
    pub abs_path: Option<String>,
    pub lineno: Option<i64>,
    pub colno: Option<i64>,
    pub in_app: bool,
    pub pre_context: Vec<String>,
    pub context_line: Option<String>,
    pub post_context: Vec<String>,
    /// Where the frame was in the minified file, when a source map moved
    /// it, as `file:line:column`.
    pub minified: Option<String>,
}

#[derive(Debug, PartialEq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct Breadcrumb {
    pub timestamp: Option<String>,
    #[serde(rename = "type")]
    #[cfg_attr(test, ts(rename = "type"))]
    pub ty: Option<String>,
    pub category: Option<String>,
    pub level: Option<String>,
    pub message: Option<String>,
    pub data: Cut<Pair>,
}

/// A value that was not a string is its JSON text.
#[derive(Debug, PartialEq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct Pair {
    pub key: String,
    pub value: String,
}

#[derive(Debug, PartialEq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct Request {
    pub method: Option<String>,
    pub url: Option<String>,
}

#[derive(Debug, PartialEq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct Context {
    pub name: String,
    pub fields: Cut<Pair>,
}

/// The view of the stored event `raw`.
pub fn view(raw: &Value) -> EventView {
    let request = &raw["request"];
    let request = (!request["method"].is_null() || !request["url"].is_null()).then(|| Request {
        method: text(&request["method"]),
        url: text(&request["url"]),
    });
    EventView {
        event_id: text(&raw["event_id"]).unwrap_or_default(),
        timestamp: timestamp(raw.get("timestamp")).map(time::rfc3339_ms),
        level: raw["level"]
            .as_str()
            .map(Level::parse)
            .unwrap_or_default()
            .as_str()
            .to_owned(),
        platform: text(&raw["platform"]),
        release: text(&raw["release"]),
        environment: text(&raw["environment"]),
        message: message(raw).map(|m| cut_string(&m)),
        trace_id: raw["contexts"]["trace"]["trace_id"]
            .as_str()
            .and_then(|s| crate::logs::hex_id(s, 32)),
        exceptions: last(values(&raw["exception"]), MAX_EXCEPTIONS, exception),
        breadcrumbs: last(values(&raw["breadcrumbs"]), MAX_BREADCRUMBS, breadcrumb),
        tags: tags(&raw["tags"]),
        request,
        user: fields(&raw["user"], &[]),
        contexts: contexts(&raw["contexts"]),
    }
}

fn exception(x: &Value) -> ExceptionView {
    let frames = x["stacktrace"]["frames"]
        .as_array()
        .map_or(&[][..], Vec::as_slice);
    ExceptionView {
        ty: text(&x["type"]),
        value: text(&x["value"]),
        mechanism: text(&x["mechanism"]["type"]),
        handled: x["mechanism"]["handled"].as_bool(),
        frames: last(frames, MAX_FRAMES, frame),
    }
}

fn frame(f: &Value) -> FrameView {
    let lines = |v: &Value| -> Vec<String> {
        v.as_array()
            .map_or(&[][..], Vec::as_slice)
            .iter()
            .filter_map(text)
            .collect()
    };
    let mut pre_context = lines(&f["pre_context"]);
    pre_context.drain(..pre_context.len().saturating_sub(MAX_CONTEXT_LINES));
    let mut post_context = lines(&f["post_context"]);
    post_context.truncate(MAX_CONTEXT_LINES);
    let minified = &f["data"]["minified"];
    let minified = minified.is_object().then(|| {
        let file = text(&minified["abs_path"])
            .or_else(|| text(&minified["filename"]))
            .unwrap_or_default();
        let number = |k: &str| {
            minified[k]
                .as_i64()
                .map_or_else(String::new, |n| n.to_string())
        };
        cut_string(&format!("{file}:{}:{}", number("lineno"), number("colno")))
    });
    FrameView {
        function: text(&f["function"]),
        module: text(&f["module"]),
        filename: text(&f["filename"]),
        abs_path: text(&f["abs_path"]),
        lineno: f["lineno"].as_i64(),
        colno: f["colno"].as_i64(),
        in_app: f["in_app"] == true,
        pre_context,
        context_line: text(&f["context_line"]),
        post_context,
        minified,
    }
}

fn breadcrumb(b: &Value) -> Breadcrumb {
    Breadcrumb {
        timestamp: timestamp(b.get("timestamp")).map(time::rfc3339_ms),
        ty: text(&b["type"]),
        category: text(&b["category"]),
        level: text(&b["level"]),
        message: text(&b["message"]),
        data: fields(&b["data"], &[]),
    }
}

/// Tags come as an object or as a list of `[key, value]` pairs.
fn tags(v: &Value) -> Cut<Pair> {
    match v {
        Value::Array(pairs) => {
            let all = pairs.iter().filter_map(|p| Some((text(&p[0])?, &p[1])));
            limit(all.map(|(key, value)| pair(key, value)), MAX_PAIRS)
        }
        other => fields(other, &[]),
    }
}

fn contexts(v: &Value) -> Cut<Context> {
    const FIRST: [&str; 3] = ["runtime", "os", "browser"];
    let Some(all) = v.as_object() else {
        return Cut {
            items: Vec::new(),
            more: 0,
        };
    };
    let rest = all
        .iter()
        .filter(|(name, _)| !FIRST.contains(&name.as_str()) && *name != "trace");
    let first = FIRST.iter().filter_map(|name| all.get_key_value(*name));
    let context = |(name, fields_of): (&String, &Value)| Context {
        name: cut_string(name),
        fields: fields(fields_of, &["type"]),
    };
    limit(first.chain(rest).map(context), MAX_CONTEXTS)
}

/// The fields of an object but `skip`, as pairs.
fn fields(v: &Value, skip: &[&str]) -> Cut<Pair> {
    let all = v
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(key, value)| !skip.contains(&key.as_str()) && !value.is_null());
    limit(
        all.map(|(key, value)| pair(cut_string(key), value)),
        MAX_PAIRS,
    )
}

fn pair(key: String, value: &Value) -> Pair {
    Pair {
        key,
        value: text(value).unwrap_or_default(),
    }
}

/// The last `n` of `all`, each made by `f`.
fn last<T>(all: &[Value], n: usize, f: impl Fn(&Value) -> T) -> Cut<T> {
    let more = all.len().saturating_sub(n);
    Cut {
        items: all[more..].iter().map(f).collect(),
        more,
    }
}

/// The first `n` of `all`.
fn limit<T>(all: impl Iterator<Item = T>, n: usize) -> Cut<T> {
    let mut items = Vec::new();
    let mut more = 0;
    for item in all {
        if items.len() < n {
            items.push(item);
        } else {
            more += 1;
        }
    }
    Cut { items, more }
}

/// A string as it is, any other value but null as its JSON text, cut to
/// [`MAX_STRING`] bytes.
fn text(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::String(s) => Some(cut_string(s)),
        other => Some(cut_string(&other.to_string())),
    }
}

fn cut_string(s: &str) -> String {
    if s.len() <= MAX_STRING {
        return s.to_owned();
    }
    let end = s.floor_char_boundary(MAX_STRING);
    format!("{}…[{} more bytes]", &s[..end], s.len() - end)
}

#[cfg(test)]
#[path = "event_view_tests.rs"]
mod tests;
