use super::*;
use crate::test_support::{fixture, scratch};
use std::fmt::Write as _;

/// 2026-01-01T00:00:00Z.
const AT_MS: i64 = 1_767_225_600_000;
const ID: &str = "0123456789abcdef0123456789abcdef";

/// What `Backtrace::force_capture` printed in the panic hook of `davy
/// crash`, as `e2e/fixtures/davy-backtrace/README.md` says.
fn captured(name: &str) -> String {
    String::from_utf8(fixture(&format!("davy-backtrace/{name}"))).unwrap()
}

const LINUX: &str = "linux-aarch64-musl.txt";
const MACOS: &str = "macos-aarch64.txt";

fn frame(function: &str, file: Option<&str>, line: Option<u32>, column: Option<u32>) -> Frame {
    Frame {
        function: function.to_owned(),
        file: file.map(str::to_owned),
        line,
        column,
    }
}

fn report(message: &str, frames: Vec<Frame>) -> Report {
    Report {
        format: FORMAT,
        event_id: ID.to_owned(),
        at_ms: AT_MS,
        release: "davy@1.2.3".to_owned(),
        thread: "log writer".to_owned(),
        message: message.to_owned(),
        location: Some("src/logs/store.rs:4:55".to_owned()),
        frames,
    }
}

#[test]
fn a_linux_backtrace_keeps_the_frames_between_the_panic_machinery_and_the_thread_start() {
    let kept = frames(&captured(LINUX));
    let tokio = |file: &str, line: u32| {
        let path =
            format!("/cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.54.1/src/{file}");
        (path, line)
    };
    let shown: Vec<(&str, Option<&str>, Option<u32>)> = kept
        .iter()
        .map(|f| (f.function.as_str(), f.file.as_deref(), f.line))
        .collect();
    assert_eq!(shown.len(), 14, "{shown:#?}");
    assert_eq!(
        shown[..2],
        [
            (
                "call_once<fn() -> core::result::Result<(), anyhow::Error>, ()>",
                Some(
                    "/rustc/58cf8f9c35b355199727627d07ae34a4e2846d67/library/core/src/ops/function.rs"
                ),
                Some(250)
            ),
            ("main", Some("src/main.rs"), Some(346)),
        ]
    );
    let (park, line) = tokio("runtime/park.rs", 376);
    assert_eq!(
        shown[11..],
        [
            (
                "{closure#0}<davy::main::{async_block_env#0}>",
                Some(park.as_str()),
                Some(line)
            ),
            ("{async_block#0}", Some("src/main.rs"), Some(314)),
            (
                "davy::self_report::crash::test_panic",
                Some("src/self_report/crash.rs"),
                Some(134)
            ),
        ]
    );
}

#[test]
fn the_files_of_a_build_that_kept_its_path_are_named_from_the_crate_root() {
    // A build without remapping, run from another directory than the one
    // it was built in, names Davy's files with the absolute path.
    let text = captured(LINUX).replace(" ./src/", " /home/op/build/davy/src/");
    let kept = frames(&text);
    let own: Vec<_> = kept
        .iter()
        .filter_map(|f| f.file.as_deref().filter(|file| own_file(file)))
        .collect();
    assert_eq!(
        own,
        ["src/main.rs", "src/main.rs", "src/self_report/crash.rs"]
    );
    assert!(
        kept.iter()
            .all(|f| !f.file.as_deref().unwrap_or("").contains("/home/op"))
    );
}

#[test]
fn a_frame_without_a_symbol_or_a_location_is_kept() {
    let text = "   0: davy::f
   1: 0x10203 - <unknown>
   2: davy::g
             at ./src/g.rs:3:1
";
    assert_eq!(
        frames(text),
        [
            frame("davy::g", Some("src/g.rs"), Some(3), Some(1)),
            frame("<unknown>", None, None, None),
            frame("davy::f", None, None, None),
        ]
    );
}

#[test]
fn a_backtrace_without_panic_machinery_or_thread_start_is_kept_whole() {
    let text = "   0: davy::main\n             at src/main.rs:7\n   1: main\n             at C:\\davy\\src\\main.rs\n";
    assert_eq!(
        frames(text),
        [
            frame("main", Some("C:\\davy\\src\\main.rs"), None, None),
            frame("davy::main", Some("src/main.rs"), Some(7), None),
        ]
    );
}

#[test]
fn a_backtrace_keeps_its_innermost_frames() {
    let mut text = String::new();
    for i in 0..150 {
        writeln!(text, "  {i}: davy::f{i}").unwrap();
    }
    let kept = frames(&text);
    assert_eq!(kept.len(), MAX_FRAMES);
    assert_eq!(kept[0].function, "davy::f99");
    assert_eq!(kept[99].function, "davy::f0");
}

#[test]
fn a_report_caps_its_message_and_names() {
    let long = "é".repeat(MAX_MESSAGE);
    let r = Report::new(&long, None, &"t".repeat(600), "", AT_MS, ID.to_owned());
    assert_eq!(r.message.len(), MAX_MESSAGE);
    assert_eq!(r.thread.len(), 512);
    assert_eq!(r.release, format!("davy@{}", env!("CARGO_PKG_VERSION")));
    assert_eq!(r.frames, []);
}

#[test]
fn a_crash_file_is_written_for_its_owner_and_read_back() {
    let dir = scratch("crash-write");
    let r = report("no value", frames(&captured(LINUX)));
    let path = write(&dir, &r).unwrap();
    assert_eq!(
        path.file_name().unwrap().to_str().unwrap(),
        "1767225600000-0123456789abcdef0123456789abcdef.json"
    );
    assert_eq!(read(&path).unwrap(), r);
    assert_eq!(pending(&dir).unwrap(), std::slice::from_ref(&path));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&path), 0o600);
    }
    let names: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names.len(), 1, "no hidden file is left: {names:?}");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn no_more_than_twenty_crash_files_wait() {
    let dir = scratch("crash-bound");
    for i in 0..MAX_PENDING {
        let r = Report {
            at_ms: AT_MS + i64::try_from(i).unwrap(),
            ..report("no value", Vec::new())
        };
        write(&dir, &r).unwrap();
    }
    let e = write(&dir, &report("one too many", Vec::new())).unwrap_err();
    assert_eq!(e.to_string(), "20 crashes wait to be recorded already");
    let files = pending(&dir).unwrap();
    assert_eq!(files.len(), 20);
    assert!(files[0].ends_with("1767225600000-0123456789abcdef0123456789abcdef.json"));
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_crash_file_keeps_the_innermost_frames_that_fit() {
    let dir = scratch("crash-size");
    let name = |i: usize| format!("davy::{i:03}::{}", "x".repeat(MAX_NAME - 12));
    let all: Vec<Frame> = (0..MAX_FRAMES)
        .map(|i| frame(&name(i), Some(&"f".repeat(400)), Some(1), Some(1)))
        .collect();
    let path = write(&dir, &report("big", all)).unwrap();
    let size = fs::metadata(&path).unwrap().len();
    assert!(size <= 64 * 1024, "{size}");
    let kept = read(&path).unwrap().frames;
    assert_eq!(kept.len(), 68);
    assert_eq!(kept.last().unwrap().function, name(MAX_FRAMES - 1));
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_crash_file_that_is_not_ours_is_refused() {
    let dir = scratch("crash-refused");
    fs::create_dir_all(&dir).unwrap();
    let refused = |name: &str, bytes: Vec<u8>| {
        let path = dir.join(name);
        fs::write(&path, bytes).unwrap();
        format!("{:#}", read(&path).unwrap_err())
    };
    let json = |r: &Report| serde_json::to_vec(r).unwrap();
    assert_eq!(
        refused("large.json", vec![b' '; MAX_FILE + 1]),
        "larger than 65536 bytes"
    );
    assert_eq!(
        refused(
            "format.json",
            json(&Report {
                format: 2,
                ..report("x", Vec::new())
            })
        ),
        "format 2 is not 1"
    );
    let many = vec![frame("davy::f", None, None, None); MAX_FRAMES + 1];
    assert_eq!(
        refused("frames.json", json(&report("x", many))),
        "more than 100 frames"
    );
    let id = Report {
        event_id: "../../etc".to_owned(),
        ..report("x", Vec::new())
    };
    assert_eq!(refused("id.json", json(&id)), "invalid event_id");
    assert!(refused("text.json", b"not json".to_vec()).starts_with("expected ident"));
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_crash_is_a_fatal_unhandled_panic_whose_davy_frames_are_in_app() {
    let r = report("no value", frames(&captured(LINUX)));
    let mut e = event(&r);
    let stack = e["exception"]["values"][0]["stacktrace"]["frames"].take();
    assert_eq!(
        e,
        json!({
            "event_id": ID,
            "timestamp": "2026-01-01T00:00:00Z",
            "platform": "rust",
            "level": "fatal",
            "logger": "panic",
            "release": "davy@1.2.3",
            "exception": {"values": [{
                "type": "panic",
                "value": "no value",
                "mechanism": {"type": "panic", "handled": false},
                "stacktrace": {"frames": null},
            }]},
            "tags": {"thread": "log writer", "location": "src/logs/store.rs:4:55"},
            "contexts": {"runtime": {"name": "davy", "version": "1.2.3"}},
        })
    );
    let stack = stack.as_array().unwrap();
    let in_app: Vec<_> = stack
        .iter()
        .filter(|f| f["in_app"] == json!(true))
        .map(|f| {
            (
                f["function"].as_str().unwrap(),
                f["filename"].as_str().unwrap(),
                f["lineno"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        in_app,
        [
            ("main", "src/main.rs", 346),
            ("{async_block#0}", "src/main.rs", 314),
            (
                "davy::self_report::crash::test_panic",
                "src/self_report/crash.rs",
                134
            ),
        ]
    );
    assert_eq!(
        stack.last().unwrap(),
        &json!({"function": "davy::self_report::crash::test_panic", "module": "davy", "filename": "src/self_report/crash.rs", "lineno": 134, "colno": 5, "in_app": true})
    );
    let tokio = stack
        .iter()
        .find(|f| f["function"].as_str().unwrap().starts_with("block_on<"))
        .unwrap();
    assert_eq!(tokio["in_app"], json!(false));
    assert!(tokio.get("module").is_none(), "{tokio}");
}

#[test]
fn a_macos_crash_names_functions_only_and_davys_by_their_module() {
    let r = report("no value", frames(&captured(MACOS)));
    assert!(
        r.frames
            .iter()
            .all(|f| f.file.is_none() && f.line.is_none())
    );
    let stack = event(&r)["exception"]["values"][0]["stacktrace"]["frames"].take();
    let shown: Vec<_> = stack
        .as_array()
        .unwrap()
        .iter()
        .map(|f| (f["function"].as_str().unwrap(), f["in_app"] == json!(true)))
        .collect();
    assert_eq!(
        shown,
        [
            ("davy::main", true),
            (
                "<tokio::runtime::runtime::Runtime>::block_on::<davy::main::{closure#0}>",
                false
            ),
            (
                "tokio::runtime::context::runtime::enter_runtime::<<tokio::runtime::scheduler::multi_thread::MultiThread>::block_on<davy::main::{closure#0}>::{closure#0}, core::result::Result<(), anyhow::Error>>",
                false
            ),
            (
                "<tokio::runtime::park::CachedParkThread>::block_on::<davy::main::{closure#0}>",
                false
            ),
            ("davy::main::{closure#0}", true),
            ("davy::self_report::crash::test_panic", true),
        ]
    );
    assert_eq!(
        stack.as_array().unwrap().last().unwrap(),
        &json!({"function": "davy::self_report::crash::test_panic", "module": "davy", "in_app": true})
    );
}

#[test]
fn the_crate_of_a_function_is_its_module() {
    assert_eq!(module("davy::serve::run"), Some("davy"));
    assert_eq!(module("<&dyn tokio::io::AsyncRead>::poll"), Some("tokio"));
    assert_eq!(module("main"), None);
    assert_eq!(module("block_on<davy::main::{async_block_env#0}>"), None);
    assert_eq!(module("{closure#0}<davy::main::{async_block_env#0}>"), None);
}

#[test]
fn a_frame_inlined_in_a_release_build_is_davys_by_its_file() {
    let at = |function: &str, file: &str| Frame {
        function: function.to_owned(),
        file: Some(file.to_owned()),
        line: Some(126),
        column: Some(5),
    };
    let own = super::frame(&at("test_panic", "src/self_report/crash.rs"));
    assert_eq!(
        own,
        json!({"function": "test_panic", "module": "davy", "filename": "src/self_report/crash.rs", "lineno": 126, "colno": 5, "in_app": true})
    );
    let windows = super::frame(&at("main", r"src\main.rs"));
    assert_eq!(windows["in_app"], json!(true));
    let tokio = super::frame(&at(
        "block_on<davy::main::{async_block_env#0}>",
        "/Users/x/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.54.1/src/runtime/park.rs",
    ));
    assert_eq!(
        tokio,
        json!({"function": "block_on<davy::main::{async_block_env#0}>", "filename": "/Users/x/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.54.1/src/runtime/park.rs", "lineno": 126, "colno": 5, "in_app": false})
    );
}
