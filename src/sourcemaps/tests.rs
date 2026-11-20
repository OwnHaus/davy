use super::{Build, DebugId, symbolicate};
use crate::envelope::{Envelope, ItemKind};
use serde_json::{Value, json};
use sourcemap::{SourceMap, SourceView};
use std::sync::Arc;

const BUILD: &str = "1c85f8bb-fda9-501c-b106-0fdc0d6937a8";
const MINIFIED: &str = "/home/dev/.t3/projects/davy/.scratch/m5/work/dist/app.min.js";

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/e2e/fixtures/sentry-cli-3/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn captured_event() -> Value {
    let envelope = Envelope::parse(&fixture("06.body")).unwrap();
    let item = envelope
        .items
        .iter()
        .find(|i| i.kind == ItemKind::Event)
        .unwrap();
    serde_json::from_slice(&item.payload).unwrap()
}

fn captured_build() -> Arc<Build> {
    Arc::new(Build {
        map: SourceMap::from_slice(&fixture("app.min.js.map")).unwrap(),
        minified: Some(SourceView::new(
            String::from_utf8(fixture("app.min.js")).unwrap().into(),
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
    symbolicate(&mut event, |id| {
        (id == DebugId::parse(BUILD).unwrap()).then(|| Arc::clone(&build))
    });
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
    symbolicate(&mut event, |_| None);
    assert_eq!(event, before);
}

#[test]
fn a_frame_from_another_file_stays_as_it_is() {
    let mut event = captured_event();
    let before = frames(&event)[1].clone();
    let build = captured_build();
    symbolicate(&mut event, |_| Some(Arc::clone(&build)));
    assert_eq!(frames(&event)[1], before);
}

#[test]
fn an_event_looks_up_each_build_once_and_symbolicates_at_most_256_frames() {
    let mut event = captured_event();
    let minified = frames(&event)[3].clone();
    event["exception"]["values"][0]["stacktrace"]["frames"] = json!(vec![minified; 300]);
    let build = captured_build();
    let lookups = std::cell::Cell::new(0);
    symbolicate(&mut event, |_| {
        lookups.set(lookups.get() + 1);
        Some(Arc::clone(&build))
    });
    assert_eq!(lookups.get(), 1);
    let frames = frames(&event);
    assert_eq!(
        [&frames[43]["function"], &frames[44]["function"]],
        [&json!("n"), &json!("parseOrder")]
    );
}
