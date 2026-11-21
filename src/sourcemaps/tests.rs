use super::{
    Build, DebugId, Store,
    bundle::{self, Kind, Limit},
    symbolicate,
};
use crate::envelope::{Envelope, ItemKind};
use crate::test_support::{FREE, fixture};
use serde_json::{Value, json};
use sourcemap::{SourceMap, SourceView};
use std::{io::Write, sync::Arc};
use zip::{ZipWriter, write::SimpleFileOptions};

const BUILD: &str = "1c85f8bb-fda9-501c-b106-0fdc0d6937a8";
const MINIFIED: &str = "/home/dev/.t3/projects/davy/.scratch/m5/work/dist/app.min.js";

fn captured_event() -> Value {
    let body = fixture("sentry-cli-3/06.body");
    let envelope = Envelope::parse(&body).unwrap();
    let item = envelope
        .items
        .iter()
        .find(|i| i.kind == ItemKind::Event)
        .unwrap();
    serde_json::from_slice(item.payload).unwrap()
}

fn captured_build() -> Arc<Build> {
    Arc::new(Build {
        map: SourceMap::from_slice(&fixture("sentry-cli-3/app.min.js.map")).unwrap(),
        minified: Some(SourceView::new(
            String::from_utf8(fixture("sentry-cli-3/app.min.js"))
                .unwrap()
                .into(),
        )),
    })
}

fn frames(event: &Value) -> &Vec<Value> {
    event["exception"]["values"][0]["stacktrace"]["frames"]
        .as_array()
        .unwrap()
}

#[test]
fn minified_frames_get_the_original_function_file_line_and_source() {
    let mut event = captured_event();
    let build = captured_build();
    symbolicate(
        &mut event,
        |id| (id == DebugId::parse(BUILD).unwrap()).then(|| Arc::clone(&build)),
        FREE,
    )
    .unwrap();
    let frames = frames(&event);
    assert_eq!(
        frames[3],
        json!({
            "filename": "../src/app.js",
            "function": "parseOrder",
            "lineno": 5,
            "colno": 16,
            "in_app": true,
            "pre_context": ["import * as Sentry from 'sentry-node-10'", "", "function parseOrder(raw) {", "  const order = JSON.parse(raw)"],
            "context_line": "  return order.items.length",
            "post_context": ["}", "", "export async function run(dsn) {", "  Sentry.init({ dsn, release: 'shop@2.0.0', environment: 'test' })", "  try {"],
            "data": {"minified": {"filename": MINIFIED, "module": "app.min", "function": "n", "lineno": 3, "colno": 74}},
        })
    );
    assert_eq!(
        [
            &frames[2]["function"],
            &frames[2]["lineno"],
            &frames[2]["context_line"]
        ],
        [
            &json!("run"),
            &json!(11),
            &json!("    parseOrder('{\"id\": 7}')")
        ]
    );
}

#[test]
fn frames_without_a_known_build_stay_as_they_are() {
    let mut event = captured_event();
    let before = event.clone();
    symbolicate(&mut event, |_| None, FREE).unwrap();
    assert_eq!(event, before);
}

#[test]
fn a_frame_from_another_file_stays_as_it_is() {
    let mut event = captured_event();
    let before = frames(&event)[1].clone();
    let build = captured_build();
    symbolicate(&mut event, |_| Some(Arc::clone(&build)), FREE).unwrap();
    assert_eq!(frames(&event)[1], before);
}

#[test]
fn an_event_looks_up_each_build_once_and_symbolicates_at_most_256_frames() {
    let mut event = captured_event();
    let minified = frames(&event)[3].clone();
    event["exception"]["values"][0]["stacktrace"]["frames"] = json!(vec![minified; 300]);
    let build = captured_build();
    let lookups = std::cell::Cell::new(0);
    symbolicate(
        &mut event,
        |_| {
            lookups.set(lookups.get() + 1);
            Some(Arc::clone(&build))
        },
        FREE,
    )
    .unwrap();
    assert_eq!(lookups.get(), 1);
    let frames = frames(&event);
    assert_eq!(
        [&frames[43]["function"], &frames[44]["function"]],
        [&json!("n"), &json!("parseOrder")]
    );
}

#[test]
fn a_context_line_keeps_256_bytes_around_the_column() {
    let mut map: Value = serde_json::from_slice(&fixture("sentry-cli-3/app.min.js.map")).unwrap();
    let app = map["sources"]
        .as_array()
        .unwrap()
        .iter()
        .position(|s| s == "../src/app.js")
        .unwrap();
    let source = map["sourcesContent"][app].as_str().unwrap();
    let long = source.replacen(
        "  return order.items.length",
        &format!("  return order.items.length{}", "x".repeat(1 << 20)),
        1,
    );
    map["sourcesContent"][app] = json!(long);
    let build = Arc::new(Build::parse(&serde_json::to_vec(&map).unwrap(), None).unwrap());
    let mut event = captured_event();
    symbolicate(&mut event, |_| Some(Arc::clone(&build)), FREE).unwrap();
    let line = format!("  return order.items.length{}", "x".repeat(1 << 20));
    assert_eq!(
        frames(&event)[3]["context_line"],
        json!(format!("{}…", &line[..256]))
    );
}

#[test]
fn each_symbolicated_frame_pays_for_the_heap_it_adds() {
    let mut event = captured_event();
    let before = crate::heap::of_value(&event);
    let build = captured_build();
    let mut paid = Vec::new();
    symbolicate(
        &mut event,
        |_| Some(Arc::clone(&build)),
        |bytes| {
            paid.push(bytes);
            Ok::<_, ()>(())
        },
    )
    .unwrap();
    assert_eq!(paid.len(), 2);
    assert!(paid.iter().all(|&bytes| bytes > 0));
    assert_eq!(
        before + paid.iter().sum::<usize>(),
        crate::heap::of_value(&event)
    );
}

#[test]
fn a_refused_payment_stops_symbolication() {
    let mut event = captured_event();
    let build = captured_build();
    let refused = symbolicate(&mut event, |_| Some(Arc::clone(&build)), |_| Err("busy"));
    assert_eq!(refused, Err("busy"));
    let frames = frames(&event);
    assert_eq!(
        [&frames[2]["function"], &frames[3]["function"]],
        [&json!("o"), &json!("parseOrder")]
    );
}

#[test]
fn requests_that_miss_the_cache_together_parse_a_build_once() {
    let store = Arc::new(Store::open(&crate::test_support::scratch("parse-once")));
    let id = DebugId::parse(BUILD).unwrap();
    let map = zstd::encode_all(&fixture("sentry-cli-3/app.min.js.map")[..], 3).unwrap();
    super::write_atomic(&store.path(id, Kind::Map), &map).unwrap();
    let start = Arc::new(std::sync::Barrier::new(8));
    let builds: Vec<Arc<Build>> = (0..8)
        .map(|_| {
            let (store, start) = (Arc::clone(&store), Arc::clone(&start));
            std::thread::spawn(move || {
                start.wait();
                store.build(id, super::parse_deadline()).unwrap().unwrap()
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|t| t.join().unwrap())
        .collect();
    assert!(builds.iter().all(|b| Arc::ptr_eq(b, &builds[0])));
}

fn zip(manifest: &Value, files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut zip = ZipWriter::new(std::io::Cursor::new(Vec::new()));
    zip.start_file("manifest.json", SimpleFileOptions::default())
        .unwrap();
    zip.write_all(manifest.to_string().as_bytes()).unwrap();
    for (path, bytes) in files {
        zip.start_file(*path, SimpleFileOptions::default()).unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

fn source_map(n: usize) -> (String, Value) {
    (
        format!("files/{n}.map"),
        json!({"type": "source_map", "headers": {"debug-id": format!("1c85f8bb-fda9-501c-b106-{n:012x}")}}),
    )
}

#[test]
fn a_bundle_listing_more_than_10000_files_is_refused() {
    let files: serde_json::Map<String, Value> = (0..10_001).map(source_map).collect();
    let error = bundle::unpack(&zip(&json!({ "files": files }), &[]), bundle::BUNDLE).unwrap_err();
    assert_eq!(error.to_string(), "bundle lists more than 10000 files");
}

#[test]
fn a_bundle_is_refused_once_it_unpacks_past_its_byte_limit() {
    let (first, second) = (source_map(1), source_map(2));
    let manifest = json!({"files": {&first.0: first.1, &second.0: second.1}});
    let zip = zip(
        &manifest,
        &[(&first.0, b"[1,2,3]"), (&second.0, b"[1,2,3]")],
    );
    let size = manifest.to_string().len() + 14;
    assert_eq!(
        bundle::unpack(
            &zip,
            Limit {
                files: 2,
                bytes: size - 1
            }
        )
        .unwrap_err()
        .to_string(),
        format!("bundle unpacks to more than {} bytes", size - 1)
    );
    assert_eq!(
        bundle::unpack(
            &zip,
            Limit {
                files: 2,
                bytes: size
            }
        )
        .unwrap()
        .len(),
        2
    );
}

#[test]
fn an_event_looks_up_at_most_8_builds() {
    let ids: Vec<String> = (0..9)
        .map(|i| format!("00000000-0000-4000-8000-00000000000{i}"))
        .collect();
    let mut event = json!({
        "debug_meta": {"images": ids.iter().enumerate().map(|(i, id)| json!({
            "type": "sourcemap", "code_file": format!("/{i}.js"), "debug_id": id,
        })).collect::<Vec<_>>()},
        "exception": {"values": [{"stacktrace": {"frames": (0..9).map(|i| json!({
            "filename": format!("/{i}.js"), "lineno": 1, "colno": 1,
        })).collect::<Vec<_>>()}}]},
    });
    let looked_up = std::cell::RefCell::new(Vec::new());
    symbolicate(
        &mut event,
        |id| {
            looked_up.borrow_mut().push(id);
            None
        },
        FREE,
    )
    .unwrap();
    let newest: Vec<DebugId> = ids[1..]
        .iter()
        .rev()
        .map(|id| DebugId::parse(id).unwrap())
        .collect();
    assert_eq!(looked_up.into_inner(), newest);
}

#[test]
fn a_build_gives_up_when_the_parse_lock_stays_busy_past_its_deadline() {
    let store = Store::open(&crate::test_support::scratch("parse-deadline"));
    let _busy = store.parsing.lock().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(50);
    let Err(err) = store.build(DebugId::parse(BUILD).unwrap(), deadline) else {
        panic!("built while another parse held the lock");
    };
    assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
}
