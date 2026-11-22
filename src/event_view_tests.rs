use super::*;
use crate::test_support::fixture;
use serde_json::json;

/// The event item of an envelope the Sentry Node SDK 10 sent.
fn node_event() -> Value {
    let body = fixture("sentry-node-10/4.body");
    let event = String::from_utf8(body)
        .unwrap()
        .lines()
        .nth(2)
        .unwrap()
        .to_owned();
    serde_json::from_str(&event).unwrap()
}

fn pairs(items: &[(&str, &str)]) -> Vec<Pair> {
    items
        .iter()
        .map(|(key, value)| Pair {
            key: (*key).to_owned(),
            value: (*value).to_owned(),
        })
        .collect()
}

#[test]
fn a_node_event_shows_its_chain_and_frames() {
    let v = view(&node_event());
    assert_eq!(v.event_id, "5ead9abbc8f44f31b35d8a435f101579");
    assert_eq!(v.timestamp.as_deref(), Some("2026-11-20T08:42:01.048Z"));
    assert_eq!(
        (
            v.level.as_str(),
            v.platform.as_deref(),
            v.release.as_deref(),
            v.environment.as_deref()
        ),
        ("error", Some("node"), Some("e2e@1.0.0"), Some("test"))
    );
    assert_eq!(
        v.trace_id.as_deref(),
        Some("f9e1079522134d8194d62046d471f90c")
    );
    assert_eq!(v.message, None);
    assert_eq!(v.exceptions.more, 0);
    let [x] = v.exceptions.items.as_slice() else {
        panic!("one exception, got {:?}", v.exceptions.items);
    };
    assert_eq!(x.ty.as_deref(), Some("SyntaxError"));
    assert_eq!(
        x.value.as_deref(),
        Some("Expected property name or '}' in JSON at position 1 (line 1 column 2)")
    );
    assert_eq!(
        (x.mechanism.as_deref(), x.handled),
        (Some("generic"), Some(true))
    );
    let functions: Vec<_> = x
        .frames
        .items
        .iter()
        .map(|f| f.function.as_deref())
        .collect();
    assert_eq!(
        functions,
        [
            Some("process.processTicksAndRejections"),
            Some("Server.?"),
            Some("JSON.parse")
        ]
    );
    let thrown_in = &x.frames.items[1];
    assert!(thrown_in.in_app);
    assert_eq!((thrown_in.lineno, thrown_in.colno), (Some(31), Some(10)));
    assert_eq!(
        thrown_in.context_line.as_deref(),
        Some("    JSON.parse('{nope')")
    );
    assert_eq!(thrown_in.pre_context.len(), 7);
    assert_eq!(thrown_in.post_context[0], "  } catch (e) {");
    assert_eq!(thrown_in.minified, None);
}

#[test]
fn a_node_event_shows_its_user_and_runtime() {
    let v = view(&node_event());
    assert_eq!(
        v.user,
        Cut {
            items: pairs(&[("id", "user-42")]),
            more: 0
        }
    );
    let names: Vec<_> = v.contexts.items.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "runtime",
            "os",
            "app",
            "cloud_resource",
            "culture",
            "device"
        ]
    );
    assert_eq!(
        v.contexts.items[0].fields,
        Cut {
            items: pairs(&[("name", "node"), ("version", "v24.22.2")]),
            more: 0
        }
    );
    assert_eq!(
        (v.tags.items.len(), v.breadcrumbs.items.len(), v.request),
        (0, 0, None)
    );
}

#[test]
fn long_lists_keep_the_frames_and_breadcrumbs_nearest_the_error() {
    let frames: Vec<_> = (0..300)
        .map(|n| json!({ "function": format!("f{n}") }))
        .collect();
    let crumbs: Vec<_> = (0..250)
        .map(|n| json!({ "message": format!("step {n}") }))
        .collect();
    let tags: serde_json::Map<_, _> = (0..210).map(|n| (format!("t{n:03}"), json!("x"))).collect();
    let v = view(&json!({
        "exception": { "values": [{ "type": "E", "stacktrace": { "frames": frames } }] },
        "breadcrumbs": { "values": crumbs },
        "tags": tags,
    }));
    let kept = &v.exceptions.items[0].frames;
    assert_eq!((kept.items.len(), kept.more), (256, 44));
    assert_eq!(kept.items[0].function.as_deref(), Some("f44"));
    assert_eq!(kept.items[255].function.as_deref(), Some("f299"));
    assert_eq!((v.breadcrumbs.items.len(), v.breadcrumbs.more), (200, 50));
    assert_eq!(v.breadcrumbs.items[0].message.as_deref(), Some("step 50"));
    assert_eq!((v.tags.items.len(), v.tags.more), (200, 10));
}

#[test]
fn a_long_string_is_cut_on_a_character_with_the_count_of_the_rest() {
    let long = format!("a{}", "é".repeat(40_000));
    let v = view(&json!({ "message": long }));
    let message = v.message.unwrap();
    let (kept, rest) = message.split_once('…').unwrap();
    assert_eq!(kept.len(), 65_535);
    assert_eq!(rest, "[14466 more bytes]");
}

#[test]
fn values_that_are_not_strings_show_as_json() {
    let v = view(&json!({
        "tags": [["browser", "Firefox"], ["retries", 3], [1, "no key"]],
        "user": { "id": 42, "ip_address": null, "data": { "plan": "pro" } },
        "request": { "method": "POST", "url": "https://shop.example/pay" },
        "contexts": { "trace": { "trace_id": "x" }, "os": { "type": "os", "name": "Linux" } },
        "exception": [{ "type": "E", "stacktrace": { "frames": [{
            "function": "pay",
            "data": { "minified": { "abs_path": "https://shop.example/app.min.js", "lineno": 1, "colno": 2042 } },
        }] } }],
    }));
    assert_eq!(
        v.tags,
        Cut {
            items: pairs(&[("browser", "Firefox"), ("retries", "3"), ("1", "no key")]),
            more: 0
        }
    );
    assert_eq!(
        v.user.items,
        pairs(&[("data", r#"{"plan":"pro"}"#), ("id", "42")])
    );
    assert_eq!(
        v.request,
        Some(Request {
            method: Some("POST".to_owned()),
            url: Some("https://shop.example/pay".to_owned())
        })
    );
    assert_eq!(v.contexts.items.len(), 1);
    assert_eq!(
        v.contexts.items[0].fields.items,
        pairs(&[("name", "Linux")])
    );
    assert_eq!(
        v.exceptions.items[0].frames.items[0].minified.as_deref(),
        Some("https://shop.example/app.min.js:1:2042")
    );
    assert_eq!(v.level, "error");
    assert_eq!(v.event_id, "");
}
