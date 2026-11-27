use std::path::{Path, PathBuf};

/// A fresh path under `target/`, not created yet. Canonical, so it equals the
/// data directory setup and doctor resolve it to.
///
/// Most tests leave their data behind, and a failed one keeps it for a look
/// afterwards. The first call in a run removes what earlier runs left,
/// anything older than an hour, so `target/` stops growing by a folder per
/// test per run.
pub fn scratch(name: &str) -> PathBuf {
    static SWEEP: std::sync::Once = std::sync::Once::new();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    SWEEP.call_once(|| sweep(&root, std::time::Duration::from_secs(3600)));
    root.join(format!("test-{name}-{}", uuid::Uuid::new_v4()))
}

/// Removes the `test-*` folders in `root` last changed more than `age` ago.
/// Another run may be removing the same ones, so errors are ignored.
fn sweep(root: &Path, age: std::time::Duration) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|t| t.elapsed().is_ok_and(|e| e > age));
        if old && entry.file_name().to_string_lossy().starts_with("test-") {
            drop(std::fs::remove_dir_all(entry.path()));
        }
    }
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

/// Waits out the last seconds of a UTC day for the next one to begin, so
/// a test that reads today before its request and the server that reads
/// it again agree. It yields until the day turns, as only that ends it.
pub async fn away_from_midnight() {
    let start = crate::time::now_ns();
    if crate::time::DAY_NS - start.rem_euclid(crate::time::DAY_NS) < 10_000_000_000 {
        let day = crate::time::day_of(start);
        while crate::time::day_of(crate::time::now_ns()) == day {
            tokio::task::yield_now().await;
        }
    }
}

#[test]
fn sweep_removes_old_test_folders_and_nothing_else() {
    let root = scratch("sweep");
    for dir in ["test-a", "test-b/inner", "keep"] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
    }
    sweep(&root, std::time::Duration::from_secs(3600));
    assert!(root.join("test-a").exists(), "a fresh folder stays");
    sweep(&root, std::time::Duration::ZERO);
    let mut left: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    left.sort();
    assert_eq!(left, ["keep"]);
    std::fs::remove_dir_all(&root).unwrap();
}
