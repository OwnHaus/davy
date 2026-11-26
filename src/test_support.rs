use std::path::{Path, PathBuf};

/// A fresh path under `target/`, not created yet. Canonical, so it equals the
/// data directory setup and doctor resolve it to.
pub fn scratch(name: &str) -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
    std::fs::create_dir_all(&root).unwrap();
    root.canonicalize()
        .unwrap()
        .join(format!("test-{name}-{}", uuid::Uuid::new_v4()))
}

pub fn fixture(path: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("e2e/fixtures")
            .join(path),
    )
    .unwrap()
}

/// A `pay` for code that pays for its memory as it goes, when no budget is
/// kept.
pub const FREE: fn(usize) -> Result<(), std::convert::Infallible> = |_| Ok(());

/// The fields `ty` names in `ui/src/api/usage.ts`, the types of the usage
/// page written by hand, with `?` for a field that may be left out. A test
/// checks them against the JSON the server makes, so a field renamed on
/// one side fails until the other follows.
pub fn usage_ts_fields(ty: &str) -> Vec<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("ui/src/api/usage.ts");
    let file = std::fs::read_to_string(path).unwrap();
    let start = format!("export type {ty} = {{");
    let at = file.find(&start).unwrap_or_else(|| panic!("no type {ty}")) + start.len();
    let (mut fields, mut depth, mut key, mut want) = (Vec::new(), 1, String::new(), true);
    for c in file[at..].chars() {
        match c {
            '{' | '[' | '(' => depth += 1,
            '}' | ']' | ')' if depth == 1 => break,
            '}' | ']' | ')' => depth -= 1,
            ';' | '\n' if depth == 1 => (want, key) = (true, String::new()),
            ':' if depth == 1 && want && !key.is_empty() => {
                fields.push(std::mem::take(&mut key));
                want = false;
            }
            '?' if depth == 1 && want && !key.is_empty() => key.push('?'),
            c if depth == 1 && want && (c.is_ascii_alphanumeric() || c == '_') => key.push(c),
            c if depth == 1 && want && !c.is_whitespace() => want = false,
            _ => {}
        }
    }
    fields
}

/// Panics unless every field `ty` in `ui/src/api/usage.ts` always has is
/// a key of `v`.
pub fn assert_usage_ts_fields(ty: &str, v: &serde_json::Value) {
    let keys = v.as_object().unwrap_or_else(|| panic!("{ty}: {v}"));
    for field in usage_ts_fields(ty) {
        if !field.ends_with('?') {
            assert!(keys.contains_key(&field), "{ty}.{field} is not in {v}");
        }
    }
}
