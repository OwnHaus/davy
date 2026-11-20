use super::{Build, DebugId};
use serde_json::{Map, Value, json};
use sourcemap::{SourceMap, SourceView};
use std::{collections::HashMap, sync::Arc};

const CONTEXT_LINES: u32 = 5;
/// A frame deep in a long function sits many tokens after the function's name.
const SCAN_TOKENS: usize = 1024;

/// Rewrites the exception frames of minified files named in
/// `debug_meta.images` to original source positions, in place. A frame whose
/// build `builds` does not know stays as it is.
pub fn symbolicate(event: &mut Value, builds: impl Fn(DebugId) -> Option<Arc<Build>>) {
    let images: HashMap<String, DebugId> = event
        .pointer("/debug_meta/images")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|image| image["type"] == "sourcemap")
        .filter_map(|image| {
            let file = image["code_file"].as_str()?;
            Some((
                file.to_owned(),
                DebugId::parse(image["debug_id"].as_str()?)?,
            ))
        })
        .collect();
    if images.is_empty() {
        return;
    }
    let Some(exceptions) = event
        .pointer_mut("/exception/values")
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    let frames = exceptions
        .iter_mut()
        .filter_map(|x| x.pointer_mut("/stacktrace/frames"))
        .filter_map(Value::as_array_mut)
        .flatten();
    for frame in frames {
        let Some(frame) = frame.as_object_mut() else {
            continue;
        };
        let file = frame
            .get("abs_path")
            .or_else(|| frame.get("filename"))
            .and_then(Value::as_str);
        if let Some(build) = file.and_then(|f| images.get(f)).and_then(|id| builds(*id)) {
            rewrite(frame, &build);
        }
    }
}

fn rewrite(frame: &mut Map<String, Value>, build: &Build) -> Option<()> {
    let position = |key: &str| {
        let n = u32::try_from(frame.get(key)?.as_u64()?).ok()?;
        n.checked_sub(1)
    };
    let (line, col) = (position("lineno")?, position("colno")?);
    let token = build.map.lookup_token(line, col)?;
    // The lookup returns the nearest token before the position, which can sit on an earlier line.
    if token.get_dst_line() != line {
        return None;
    }
    let source = token.get_source()?.to_owned();
    let minified_name = frame.get("function").and_then(Value::as_str);
    let function = minified_name
        .zip(build.minified.as_ref())
        .and_then(|(name, view)| original_function_name(&build.map, view, line, col, name))
        .or(minified_name)
        .map(str::to_owned);
    let mut minified = Map::new();
    for key in [
        "filename", "abs_path", "module", "function", "lineno", "colno",
    ] {
        if let Some(value) = frame.remove(key) {
            minified.insert(key.to_owned(), value);
        }
    }
    for key in ["pre_context", "context_line", "post_context"] {
        frame.remove(key);
    }
    let src_line = token.get_src_line();
    frame.insert("lineno".into(), json!(src_line + 1));
    frame.insert("colno".into(), json!(token.get_src_col() + 1));
    if let Some(function) = function {
        frame.insert("function".into(), json!(function));
    }
    if let Some(view) = build.map.get_source_view(token.get_src_id()) {
        let lines = |range: std::ops::Range<u32>| -> Vec<&str> {
            range.filter_map(|n| view.get_line(n)).collect()
        };
        frame.insert(
            "pre_context".into(),
            json!(lines(src_line.saturating_sub(CONTEXT_LINES)..src_line)),
        );
        if let Some(context) = view.get_line(src_line) {
            frame.insert("context_line".into(), json!(context));
        }
        frame.insert(
            "post_context".into(),
            json!(lines(src_line + 1..src_line + 1 + CONTEXT_LINES)),
        );
    }
    if source.contains("node_modules") {
        frame.insert("in_app".into(), json!(false));
    }
    frame.insert("filename".into(), json!(source));
    if let Value::Object(data) = frame
        .entry("data")
        .or_insert_with(|| Value::Object(Map::new()))
    {
        data.insert("minified".into(), Value::Object(minified));
    }
    Some(())
}

/// The original name of the minified function `name` that holds the position
/// `line`, `col`. It is the nearest earlier token that spells `name` right
/// after the `function` keyword and has an original name in the map.
fn original_function_name<'m>(
    map: &'m SourceMap,
    minified: &SourceView,
    line: u32,
    col: u32,
    name: &str,
) -> Option<&'m str> {
    let (mut low, mut high) = (0, usize::try_from(map.get_token_count()).ok()?);
    while low < high {
        let middle = low + (high - low) / 2;
        let token = map.get_token(middle)?;
        if (token.get_dst_line(), token.get_dst_col()) <= (line, col) {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    (low.saturating_sub(SCAN_TOKENS)..low)
        .rev()
        .find_map(|index| {
            let token = map.get_token(index)?;
            let text = minified.get_line(token.get_dst_line())?;
            let (before, after) =
                text.split_at_checked(usize::try_from(token.get_dst_col()).ok()?)?;
            let declares = before
                .trim_end()
                .trim_end_matches('*')
                .trim_end()
                .ends_with("function");
            let spells = after.strip_prefix(name).is_some_and(|rest| {
                !rest.starts_with(|c: char| c.is_alphanumeric() || c == '_' || c == '$')
            });
            if declares && spells {
                token.get_name()
            } else {
                None
            }
        })
}
