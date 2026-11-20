use crate::event::{ErrorEvent, Frame};
use regex::Regex;
use std::sync::LazyLock;
pub struct Grouping {
    pub version: u16,
    pub fingerprint: String,
    pub title: String,
    pub culprit: Option<String>,
}
pub fn normalize(s: &str) -> String {
    static RULES: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
        [
            (
                r"(?i)\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b",
                "<uuid>",
            ),
            (r"[\w.+-]+@[\w.-]+\.[a-zA-Z]+", "<email>"),
            (r"\b(?:\d{1,3}\.){3}\d{1,3}\b", "<ip>"),
            (r"(?i)\b[0-9a-f]{8,}\b", "<hex>"),
            (r"\d+(?:\.\d+)?", "<number>"),
        ]
        .into_iter()
        .map(|(r, s)| (Regex::new(r).unwrap(), s))
        .collect()
    });
    RULES.iter().fold(s.to_owned(), |s, (r, replacement)| {
        r.replace_all(&s, *replacement).into_owned()
    })
}
fn filename(s: &str) -> String {
    static HASH: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)\.[0-9a-f]{8,}(\.[a-z]+)$").unwrap());
    normalize(&HASH.replace(s.split('?').next().unwrap_or(s), "$1"))
}
fn selected(frames: &[Frame]) -> Vec<&Frame> {
    let in_app = frames.iter().any(|f| f.in_app == Some(true));
    frames
        .iter()
        .filter(|f| !in_app || f.in_app == Some(true))
        .rev()
        .take(10)
        .collect()
}
pub fn group(e: &ErrorEvent) -> Grouping {
    let mut default = Vec::new();
    if e.exceptions.is_empty() {
        default.push(
            e.message_template
                .clone()
                .unwrap_or_else(|| normalize(e.message.as_deref().unwrap_or(""))),
        )
    }
    for ex in &e.exceptions {
        default.push(ex.ty.clone());
        let frames = selected(&ex.frames);
        if frames.is_empty() {
            default.push(normalize(ex.value.as_deref().unwrap_or("")));
        }
        for f in frames.into_iter().rev() {
            default.push(f.module.clone().unwrap_or_else(|| {
                filename(
                    f.filename
                        .as_deref()
                        .or(f.abs_path.as_deref())
                        .unwrap_or(""),
                )
            }));
            default.push(f.function.clone().unwrap_or_default());
        }
    }
    let components = match &e.fingerprint {
        None => default,
        Some(entries) => entries
            .iter()
            .flat_map(|s| {
                if s == "{{ default }}" {
                    default.clone()
                } else {
                    vec![s.clone()]
                }
            })
            .collect(),
    };
    let hash = blake3::hash(&serde_json::to_vec(&components).unwrap());
    let title = e
        .exceptions
        .last()
        .map(|ex| format!("{}: {}", ex.ty, ex.value.as_deref().unwrap_or("")))
        .or_else(|| e.message.clone())
        .unwrap_or_default()
        .chars()
        .take(200)
        .collect();
    let culprit = e
        .exceptions
        .iter()
        .rev()
        .flat_map(|ex| ex.frames.iter().rev())
        .find(|f| f.in_app == Some(true))
        .and_then(|f| f.function.clone());
    Grouping {
        version: 1,
        fingerprint: hash.to_hex()[..32].to_owned(),
        title,
        culprit,
    }
}
