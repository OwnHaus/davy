//! The heap estimates are upper bounds on what parsing allocates, measured
//! by an allocator that tracks the peak.

use davy::{
    heap,
    http::body,
    logs::{logtail, otlp, sentry},
    scrub::{Item, Meter, Policy},
    sourcemaps::Build,
};
use opentelemetry_proto::tonic::{
    collector::logs::v1::ExportLogsServiceRequest,
    common::v1::{AnyValue, ArrayValue, KeyValue, any_value::Value as Any},
    logs::v1::{LogRecord, ResourceLogs, ScopeLogs},
    resource::v1::Resource,
};
use peak_alloc::PeakAlloc;
use prost::Message;
use serde_json::Value;
use std::io::Write;

#[global_allocator]
static ALLOC: PeakAlloc = PeakAlloc;

/// The allocator counts every thread, so the tests take turns.
static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn alone() -> std::sync::MutexGuard<'static, ()> {
    ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The most memory `parse` held at once beyond what was held before it.
fn peak<T>(parse: impl FnOnce() -> T) -> usize {
    ALLOC.reset_peak_usage();
    let before = ALLOC.current_usage();
    let out = parse();
    let peak = ALLOC.peak_usage() - before;
    drop(out);
    peak
}

fn scrambled_keys(n: usize) -> String {
    let fields: Vec<String> = (0..n)
        .map(|i| format!("\"k{}\":{i}", i.wrapping_mul(7919) % n))
        .collect();
    format!("{{{}}}", fields.join(","))
}

fn json_bodies() -> Vec<(&'static str, String)> {
    let n = 100_000;
    vec![
        ("empty objects", format!("[{}{{}}]", "{},".repeat(n))),
        ("zeros", format!("[{}0]", "0,".repeat(n))),
        ("empty arrays", format!("[{}[]]", "[],".repeat(n))),
        ("short strings", format!("[{}\"\"]", "\"a\",".repeat(n))),
        (
            "ascending keys",
            format!(
                "{{{}}}",
                (0..n)
                    .map(|i| format!("\"{i:07}\":0"))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        ),
        ("scrambled keys", scrambled_keys(n)),
        (
            "one-key objects",
            format!("[{}{{}}]", "{\"a\":1},".repeat(n)),
        ),
        ("deep", format!("{}{}", "[".repeat(127), "]".repeat(127))),
        ("escaped string", format!("[\"{}\"]", "\\n".repeat(n))),
        ("escaped key", format!("{{\"{}\":0}}", "\\u00e9".repeat(n))),
    ]
}

#[test]
fn estimates_bound_the_peak_heap_of_parsing() {
    let _alone = alone();
    for (name, body) in json_bodies() {
        let body = body.as_bytes();
        let estimate = heap::json(body, 0).unwrap();
        let used = peak(|| serde_json::from_slice::<Value>(body).unwrap());
        assert!(
            used <= estimate,
            "{name} json: used {used}, estimated {estimate}"
        );
    }
}

#[test]
fn log_costs_bound_the_peak_heap_of_parsing_logs() {
    let _alone = alone();
    for (name, body) in json_bodies() {
        let body = body.as_bytes();
        if let Ok(cost) = logtail::cost(body, "application/json") {
            let used = peak(|| logtail::parse(body, "application/json", 1, 0));
            assert!(used <= cost, "{name} logtail: used {used}, cost {cost}");
        }
        let item = [&b"{\"items\":"[..], body, b"}"].concat();
        if let Ok(cost) = sentry::cost(&item) {
            let used = peak(|| sentry::parse(&item, 1, 0));
            assert!(used <= cost, "{name} sentry: used {used}, cost {cost}");
        }
    }
}

fn text(s: &str) -> AnyValue {
    AnyValue {
        value: Some(Any::StringValue(s.into())),
    }
}

fn attributes(n: usize) -> Vec<KeyValue> {
    (0..n)
        .map(|i| KeyValue {
            key: format!("k{i}"),
            value: Some(text("v")),
            ..KeyValue::default()
        })
        .collect()
}

fn otlp_requests() -> Vec<(&'static str, ExportLogsServiceRequest)> {
    let request = |resource: usize, records: Vec<LogRecord>| ExportLogsServiceRequest {
        resource_logs: vec![ResourceLogs {
            resource: Some(Resource {
                attributes: attributes(resource),
                ..Resource::default()
            }),
            scope_logs: vec![ScopeLogs {
                log_records: records,
                ..ScopeLogs::default()
            }],
            ..ResourceLogs::default()
        }],
    };
    let nested = (0..25).fold(text("leaf"), |inner, _| AnyValue {
        value: Some(Any::ArrayValue(ArrayValue {
            values: vec![inner],
        })),
    });
    vec![
        (
            "empty records",
            request(0, vec![LogRecord::default(); 20_000]),
        ),
        (
            "many attributes",
            request(
                0,
                vec![
                    LogRecord {
                        attributes: attributes(50),
                        body: Some(text("message")),
                        ..LogRecord::default()
                    };
                    500
                ],
            ),
        ),
        (
            "large resource",
            request(300, vec![LogRecord::default(); 300]),
        ),
        (
            "nested arrays",
            request(
                1,
                vec![
                    LogRecord {
                        body: Some(nested),
                        ..LogRecord::default()
                    };
                    200
                ],
            ),
        ),
    ]
}

#[test]
fn otlp_costs_bound_the_peak_heap_of_decoding_and_converting() {
    let _alone = alone();
    for (name, request) in otlp_requests() {
        let protobuf = request.encode_to_vec();
        let json = serde_json::to_vec(&request).unwrap();
        for (encoding, body) in [
            (otlp::Encoding::Protobuf, &protobuf),
            (otlp::Encoding::Json, &json),
        ] {
            let cost = otlp::cost(body, encoding).unwrap();
            let used = peak(|| otlp::decode(body, encoding).unwrap());
            assert!(
                used <= cost,
                "{name} {encoding:?}: used {used}, cost {cost}"
            );
        }
        let mut paid = 0;
        let used = peak(|| {
            otlp::convert(request, 1, 0, |bytes| {
                paid += bytes;
                Ok::<_, ()>(())
            })
        });
        assert!(used <= paid, "{name} records: used {used}, paid {paid}");
    }
}

/// Text that compresses about as well as JSON does.
fn wordy(len: usize) -> Vec<u8> {
    let words = [
        "\"message\":",
        "\"level\":\"error\",",
        "frame",
        "123",
        "{",
        "}",
        "at ",
        "\n",
    ];
    let mut x: u32 = 7;
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        out.extend_from_slice(words[x as usize % words.len()].as_bytes());
        out.extend_from_slice(&x.to_le_bytes()[..1]);
    }
    out
}

/// `payload` twice, in two metablocks, so the decoder cannot size its
/// window to a stream of one.
fn brotli(payload: &[u8], quality: u32, lgwin: u32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut w = brotli::CompressorWriter::new(&mut out, 4096, quality, lgwin);
    w.write_all(payload).expect("brotli writes");
    w.flush().expect("brotli flushes");
    w.write_all(payload).expect("brotli writes");
    drop(w);
    out
}

fn zstd(payload: &[u8], level: i32, window_log: u32) -> Vec<u8> {
    let mut e = zstd::stream::write::Encoder::new(Vec::new(), level).expect("zstd encoder");
    e.window_log(window_log).expect("window log");
    e.include_contentsize(false).expect("no content size");
    e.write_all(payload).expect("zstd writes");
    e.finish().expect("zstd finishes")
}

/// What zstd's own context holds after it decodes `body`. zstd allocates
/// with C's `malloc`, which the measuring allocator does not see.
fn zstd_context(body: &[u8]) -> usize {
    use zstd::zstd_safe::{DCtx, DParameter, InBuffer, OutBuffer};
    let mut dctx = DCtx::create();
    dctx.set_parameter(DParameter::WindowLogMax(23))
        .expect("window log max");
    let mut out = vec![0; 1 << 16];
    let mut input = InBuffer::around(body);
    loop {
        let mut output = OutBuffer::around(&mut out[..]);
        let more = dctx
            .decompress_stream(&mut output, &mut input)
            .expect("zstd decodes");
        if more == 0 && input.pos() == body.len() {
            return dctx.sizeof();
        }
    }
}

#[test]
fn a_decoder_allocates_no_more_than_its_price() {
    let _alone = alone();
    let text = wordy(256 << 10);
    let mut bodies = vec![
        ("gzip", {
            let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
            e.write_all(&text).unwrap();
            e.finish().unwrap()
        }),
        ("deflate", {
            let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
            e.write_all(&text).unwrap();
            e.finish().unwrap()
        }),
        ("br", brotli(&text, 11, 22)),
        ("zstd", zstd(&text, 19, 23)),
        ("gzip", {
            // The decoder keeps a header's extra field, file name and
            // comment, each up to 64 KiB.
            let field = vec![b'x'; 65535];
            let mut e = flate2::GzBuilder::new()
                .extra(field.clone())
                .filename(field.clone())
                .comment(field)
                .write(Vec::new(), flate2::Compression::best());
            e.write_all(&text).unwrap();
            e.finish().unwrap()
        }),
    ];
    for log in 10..=24 {
        bodies.push(("br", brotli(&text[..4096], 5, log)));
    }
    let zeros = vec![0; 16 << 20];
    for log in [10, 17, 20, 23] {
        bodies.push(("zstd", zstd(&zeros[..2 << log], 1, log)));
    }
    for (encoding, body) in bodies {
        let price = body::decoder_memory(&body, encoding);
        let mut used = peak(|| {
            let mut reader = body::decoder(&body, encoding).unwrap();
            std::io::copy(&mut reader, &mut std::io::sink()).unwrap()
        });
        if encoding == "zstd" {
            used += zstd_context(&body);
        }
        assert!(
            used <= price,
            "{encoding} of {}: used {used}, priced {price}",
            body.len()
        );
    }
}

/// Maps that each stress one part of a parse.
fn maps() -> Vec<(&'static str, String, Option<String>)> {
    let n = 100_000;
    let lines = 1 << 17;
    let map = |fields: &str| format!("{{\"version\":3,{fields}}}");
    vec![
        (
            "one-field tokens",
            map(&format!("\"mappings\":\"{}A\"", "A,".repeat(n))),
            None,
        ),
        (
            "four-field tokens",
            map(&format!(
                "\"sources\":[\"a\"],\"mappings\":\"{}AAAA\"",
                "AAAA,".repeat(n)
            )),
            None,
        ),
        (
            "empty lines",
            map(&format!("\"mappings\":\"{}\"", ";".repeat(n))),
            None,
        ),
        (
            "names",
            map(&format!("\"names\":[{}1]", "\"a\",1,".repeat(n))),
            None,
        ),
        (
            "sources with content",
            map(&format!(
                "\"sources\":[{0}\"s\"],\"sourcesContent\":[{1}\"x\"]",
                "\"s\",".repeat(n),
                "\"x\",".repeat(n)
            )),
            None,
        ),
        (
            "escaped line ends",
            map(&format!(
                "\"sources\":[\"s\"],\"sourcesContent\":[\"{}\"]",
                "\\n".repeat(lines)
            )),
            None,
        ),
        (
            "unicode line ends",
            map(&format!(
                "\"sources\":[\"s\"],\"sourcesContent\":[\"{}\"]",
                "\\u000D".repeat(lines)
            )),
            None,
        ),
        (
            "unknown fields",
            map(&format!(
                "\"x\":[{}{{}}],\"y\":\"{}\"",
                "{},".repeat(n),
                "\\n".repeat(n)
            )),
            None,
        ),
        (
            "range mappings",
            map(&format!(
                "\"mappings\":\"A\",\"rangeMappings\":\"{}\"",
                "A".repeat(n)
            )),
            None,
        ),
    ]
}

/// Builds with minified files, one of them ordinary, after [`maps`].
fn builds() -> Vec<(&'static str, String, Option<String>)> {
    let lines = 1 << 17;
    let map = |fields: &str| format!("{{\"version\":3,{fields}}}");
    let code = "  return order.items.length;\\n".repeat(lines >> 4);
    let mut builds = maps();
    builds.extend([
        (
            "ordinary build",
            map(&format!(
                "\"sources\":[\"../src/app.js\"],\"names\":[\"order\"],\"sourcesContent\":[\"{code}\"],\"mappings\":\"{}\"",
                "AAAA,CAACA,EAAE;".repeat(lines >> 4)
            )),
            Some("function o(n){return n.items.length}\n".repeat(lines >> 4)),
        ),
        (
            "minified line ends",
            map("\"mappings\":\"\""),
            Some("\n".repeat(lines)),
        ),
        (
            "minified crlf",
            map("\"mappings\":\"\""),
            Some("\r\n".repeat(lines)),
        ),
    ]);
    builds
}

#[test]
fn a_builds_price_bounds_its_parse_and_what_it_keeps() {
    let _alone = alone();
    for (name, map, minified) in builds() {
        let price = Build::price(map.as_bytes(), minified.as_ref().map(String::as_bytes)).unwrap();
        // As the store does, from the compressed files, and then through
        // every line of every file, as lookups over time can.
        let stored = zstd::encode_all(map.as_bytes(), 3).unwrap();
        let stored_minified = minified.map(|js| zstd::encode_all(js.as_bytes(), 3).unwrap());
        ALLOC.reset_peak_usage();
        let before = ALLOC.current_usage();
        let map = zstd::decode_all(stored.as_slice()).unwrap();
        let minified = stored_minified.map(|js| zstd::decode_all(js.as_slice()).unwrap());
        let parsed = sourcemap::SourceMap::from_slice(&map).unwrap();
        let view =
            minified.map(|js| sourcemap::SourceView::new(String::from_utf8(js).unwrap().into()));
        drop(map);
        let views = (0..parsed.get_source_count())
            .filter_map(|i| parsed.get_source_view(i))
            .chain(view.as_ref());
        for view in views {
            let mut line = 0;
            while view.get_line(line).is_some() {
                line += 1;
            }
        }
        let kept = ALLOC.current_usage() - before;
        let used = ALLOC.peak_usage() - before;
        drop((parsed, view));
        assert!(
            used <= price.parse,
            "{name}: parse used {used}, priced {}",
            price.parse
        );
        assert!(
            kept <= price.kept,
            "{name}: kept {kept}, priced {}",
            price.kept
        );
    }
}

/// Inputs a scrub makes larger, with the policy that does it.
fn growing() -> Vec<(&'static str, Value, Policy)> {
    let email = Policy {
        email: true,
        ..Policy::default()
    };
    let card = Value::from(4_242_424_242_424_242_u64);
    let ips: serde_json::Map<String, Value> = (0..20_000)
        .map(|i| (format!("10.0.{}.{}", i / 250, i % 250), Value::from(i)))
        .collect();
    let mut by_ip = serde_json::Map::new();
    by_ip.insert("by_ip".to_owned(), Value::Object(ips.clone()));
    let mut by_token = serde_json::Map::new();
    by_token.insert("by_token".to_owned(), Value::Object(ips));
    vec![
        (
            "credentials",
            Value::from("ghp_x ".repeat(100_000)),
            Policy::default(),
        ),
        ("addresses", Value::from("a@b.c ".repeat(100_000)), email),
        (
            "ips",
            Value::from("1.1.1.1 ".repeat(100_000)),
            Policy::default(),
        ),
        (
            "assignments",
            Value::from("pwd=x&".repeat(256)),
            Policy::default(),
        ),
        (
            "too many assignments",
            Value::from("pwd=x&".repeat(1000)),
            Policy::default(),
        ),
        (
            "long secret",
            Value::from(format!("pwd={}", "x".repeat(1 << 20))),
            Policy::default(),
        ),
        (
            "card numbers",
            Value::from(vec![card.clone(); 100_000]),
            Policy::default(),
        ),
        (
            "hidden card numbers",
            serde_json::json!({"token": vec![card; 100_000]}),
            Policy::default(),
        ),
        ("ip keys", Value::Object(by_ip), Policy::default()),
        (
            "ip keys under a secret",
            Value::Object(by_token),
            Policy::default(),
        ),
    ]
}

/// The patterns are compiled once and their caches kept by each thread, so
/// a scrub is measured after the same scrub ran once.
#[test]
fn what_a_scrub_pays_bounds_its_peak_heap() {
    let _alone = alone();
    for (name, value, policy) in growing() {
        for item in [Item::Event, Item::Session] {
            policy.item(&mut value.clone(), item, &mut Meter::unmetered());
            let mut value = value.clone();
            let mut paid = 0;
            let mut pay = |bytes: usize| {
                paid += bytes;
                true
            };
            let used = peak(|| {
                let mut meter = Meter::new(&mut pay);
                policy.item(&mut value, item, &mut meter);
                assert!(!meter.refused());
            });
            assert!(paid > 0, "{name} grows");
            assert!(used <= paid, "{name} {item:?}: used {used}, paid {paid}");
        }
    }
}
