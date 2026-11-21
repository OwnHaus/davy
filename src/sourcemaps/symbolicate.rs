use super::{Build, DebugId};
use crate::heap;
use serde_json::{Map, Value, json};
use sourcemap::{SourceMap, SourceView};
use std::{collections::HashMap, sync::Arc};

const CONTEXT_LINES: u32 = 5;
/// The longest source line a frame keeps, in bytes. A source that is itself
/// minified has lines of megabytes, and a frame copies up to eleven lines.
const MAX_LINE: usize = 256;
/// Each frame costs a token scan, so one event cannot buy unbounded CPU.
const MAX_FRAMES: usize = 256;
/// A frame deep in a long function sits many tokens after the function's name.
const SCAN_TOKENS: usize = 1024;
/// A stack runs through a few bundles. Each build an event names can cost a
/// parse and stays in memory until the event is symbolicated.
const MAX_BUILDS: usize = 8;

/// Rewrites the exception frames of minified files named in
/// `debug_meta.images` to original source positions, in place. A frame whose
/// build `builds` does not know stays as it is. `builds` runs once per debug
/// ID, for the [`MAX_BUILDS`] IDs the newest frames name. Only the newest [`MAX_FRAMES`] frames are rewritten, counted from the
/// frame that threw, because Sentry lists frames oldest first. Each frame
/// pays `pay` the heap it grew by, by the prices of [`crate::heap`], before
/// the next frame is rewritten, and a refused payment stops the rewrite.
pub fn symbolicate<E>(
    event: &mut Value,
    builds: impl Fn(DebugId) -> Option<Arc<Build>>,
    mut pay: impl FnMut(usize) -> Result<(), E>,
) -> Result<(), E> {
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
        return Ok(());
    }
    let Some(exceptions) = event
        .pointer_mut("/exception/values")
        .and_then(Value::as_array_mut)
    else {
        return Ok(());
    };
    let mut found: HashMap<DebugId, Option<Arc<Build>>> = HashMap::new();
    let frames = exceptions
        .iter_mut()
        .rev()
        .filter_map(|x| x.pointer_mut("/stacktrace/frames"))
        .filter_map(Value::as_array_mut)
        .flat_map(|frames| frames.iter_mut().rev())
        .filter_map(Value::as_object_mut)
        .filter_map(|frame| {
            let file = frame
                .get("abs_path")
                .or_else(|| frame.get("filename"))
                .and_then(Value::as_str)?;
            let id = *images.get(file)?;
            Some((frame, id))
        })
        .take(MAX_FRAMES);
    for (frame, id) in frames {
        if !found.contains_key(&id) && found.len() == MAX_BUILDS {
            continue;
        }
        if let Some(build) = found.entry(id).or_insert_with(|| builds(id)) {
            let before = heap::of_map(frame);
            rewrite(frame, build);
            pay(heap::of_map(frame).saturating_sub(before))?;
        }
    }
    Ok(())
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
    frame.insert("lineno".into(), json!(src_line.saturating_add(1)));
    frame.insert("colno".into(), json!(token.get_src_col().saturating_add(1)));
    if let Some(function) = function {
        frame.insert("function".into(), json!(function));
    }
    if let Some(view) = build.map.get_source_view(token.get_src_id()) {
        let src_col = usize::try_from(token.get_src_col()).unwrap_or(usize::MAX);
        let lines = |range: std::ops::Range<u32>| -> Vec<String> {
            range
                .filter_map(|n| view.get_line(n))
                .map(|line| trim(line, src_col))
                .collect()
        };
        frame.insert(
            "pre_context".into(),
            json!(lines(src_line.saturating_sub(CONTEXT_LINES)..src_line)),
        );
        if let Some(context) = view.get_line(src_line) {
            frame.insert("context_line".into(), json!(trim(context, src_col)));
        }
        frame.insert(
            "post_context".into(),
            json!(lines(
                src_line.saturating_add(1)..src_line.saturating_add(1 + CONTEXT_LINES)
            )),
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

/// `line` cut to [`MAX_LINE`] bytes around byte `col`, with `…` at each cut.
fn trim(line: &str, col: usize) -> String {
    if line.len() <= MAX_LINE {
        return line.to_owned();
    }
    let start =
        line.floor_char_boundary(col.saturating_sub(MAX_LINE / 2).min(line.len() - MAX_LINE));
    let end = line.floor_char_boundary(start + MAX_LINE);
    let cut = |at: bool| if at { "…" } else { "" };
    format!(
        "{}{}{}",
        cut(start > 0),
        &line[start..end],
        cut(end < line.len())
    )
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
