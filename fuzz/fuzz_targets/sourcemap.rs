#![no_main]

use davy::sourcemaps::{Build, DebugId, symbolicate};
use libfuzzer_sys::fuzz_target;
use serde_json::json;
use std::sync::Arc;

const ID: &str = "c0ffee00-0000-4000-8000-000000000001";

// Eight bytes of frame position, then the source map, then after a NUL byte
// the minified file, which is where function names are recovered from.
fuzz_target!(|data: &[u8]| {
    let Some((position, rest)) = data.split_first_chunk::<8>() else {
        return;
    };
    let line = u32::from_le_bytes([position[0], position[1], position[2], position[3]]);
    let col = u32::from_le_bytes([position[4], position[5], position[6], position[7]]);
    let (map, minified) = match rest.iter().position(|&b| b == 0) {
        Some(at) => (&rest[..at], Some(rest[at + 1..].to_vec())),
        None => (rest, None),
    };
    let Ok(build) = Build::parse(map, minified) else {
        return;
    };
    let build = Arc::new(build);
    let frame = |lineno: u32, colno: u32| json!({ "abs_path": "app.min.js", "function": "a", "lineno": lineno, "colno": colno });
    let mut event = json!({
        "debug_meta": { "images": [{ "type": "sourcemap", "code_file": "app.min.js", "debug_id": ID }] },
        "exception": { "values": [{ "stacktrace": { "frames": [
            frame(line, col),
            frame(1, 1),
            frame(line % 64 + 1, col % 4096 + 1),
        ] } }] },
    });
    let id = DebugId::parse(ID);
    let free = |_| Ok::<(), std::convert::Infallible>(());
    let Ok(()) = symbolicate(
        &mut event,
        |found| (Some(found) == id).then(|| Arc::clone(&build)),
        free,
    );
});
