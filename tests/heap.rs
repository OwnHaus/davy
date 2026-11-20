//! The heap estimates are upper bounds on what parsing allocates, measured
//! by an allocator that tracks the peak.

use davy::{
    heap,
    logs::{logtail, otlp, sentry},
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
