use super::store::{LogQuery, LogWriter, State, histogram, search};
use super::*;
use crate::test_support::{fixture, scratch};
use crate::time::{self, now_ns};
use serde_json::json;

fn gunzip(b: &[u8]) -> Vec<u8> {
    use std::io::Read;
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(b)
        .read_to_end(&mut out)
        .unwrap();
    out
}

fn attrs(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => unreachable!(),
    }
}

#[test]
fn logtail_pino_fixture() {
    let records = logtail::parse(
        &gunzip(&fixture("logtail-pino/1.body")),
        "application/msgpack",
        4,
        7,
    )
    .unwrap();
    assert_eq!(records.len(), 3);
    assert_eq!(
        records[0],
        LogRecord {
            project_id: 4,
            timestamp: 1_795_162_412_467_000_000,
            observed_at: 7,
            source: LogSource::Logtail,
            level: LogLevel::Info,
            message: "request completed".into(),
            service: None,
            trace_id: None,
            span_id: None,
            attributes: attrs(json!({
                "hostname": "api-7d9f-x1k",
                "pid": 4242,
                "req": {"id": "r-1", "method": "GET", "url": "/v1/events/1"},
                "res": {"statusCode": 200},
                "responseTime": 12
            })),
        }
    );
    assert_eq!(records[1].level, LogLevel::Error);
    assert_eq!(records[1].message, "Unhandled exception");
    assert_eq!(
        records[1].attributes["err"]["message"],
        "connect ECONNREFUSED 10.0.1.5:5432"
    );
    assert_eq!(records[2].level, LogLevel::Debug);
    assert_eq!(records[2].message, "debug line");
}

#[test]
fn logtail_json_and_ndjson() {
    let body = br#"[{"dt":"2026-11-20T08:00:00Z","message":"a","level":"warn","service":"api","trace_id":"5B8EFFF798038103D269B633813FC60C"},7,{"message":"b","level":50,"dt":"yesterday"}]"#;
    let records = logtail::parse(body, "application/json", 1, 99).unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].timestamp, 1_795_161_600_000_000_000);
    assert_eq!(records[0].level, LogLevel::Warn);
    assert_eq!(records[0].service.as_deref(), Some("api"));
    assert_eq!(
        records[0].trace_id.as_deref(),
        Some("5b8efff798038103d269b633813fc60c")
    );
    assert!(records[0].attributes.is_empty());
    assert_eq!(records[1].level, LogLevel::Error);
    assert_eq!(records[1].timestamp, 99);
    assert_eq!(
        records[1].attributes,
        attrs(json!({ORIGINAL_TIMESTAMP: "yesterday"}))
    );

    let ndjson = b"{\"message\":\"x\"}\n\n{\"message\":\"y\",\"trace_id\":\"nothex\"}\n";
    let records = logtail::parse(ndjson, "application/x-ndjson", 1, 5).unwrap();
    let messages: Vec<_> = records.iter().map(|r| r.message.as_str()).collect();
    assert_eq!(messages, ["x", "y"]);
    assert_eq!(records[1].trace_id, None);
    assert_eq!(records[1].attributes, attrs(json!({"trace_id": "nothex"})));

    let sniffed = logtail::parse(b"{\"message\":\"z\"}", "", 1, 5).unwrap();
    assert_eq!(sniffed[0].message, "z");
    assert!(logtail::parse(b"\"just a string\"", "application/json", 1, 5).is_err());
    assert!(logtail::parse(b"\xc1", "application/msgpack", 1, 5).is_err());
}

#[test]
fn an_ndjson_line_that_is_not_a_record_is_skipped() {
    let ndjson = b"{\"message\":\"x\"}\n42\n\"text\"\n{\"message\":\"y\"}\n";
    let records = logtail::parse(ndjson, "application/x-ndjson", 1, 5).unwrap();
    let messages: Vec<_> = records.iter().map(|r| r.message.as_str()).collect();
    assert_eq!(messages, ["x", "y"]);
}

#[test]
fn sentry_log_fixture() {
    let body = fixture("sentry-node-10-logs/1.body");
    let envelope = crate::envelope::Envelope::parse(&body).unwrap();
    assert_eq!(envelope.items.len(), 1);
    assert_eq!(envelope.items[0].kind, crate::envelope::ItemKind::Log);
    let records = sentry::parse(envelope.items[0].payload, 2, 7).unwrap();
    let summary: Vec<_> = records
        .iter()
        .map(|r| (r.level, r.message.as_str(), r.timestamp))
        .collect();
    assert_eq!(
        summary,
        [
            (LogLevel::Info, "Order placed", 1_795_165_575_446_429_000),
            (
                LogLevel::Warn,
                "Cart c-77 is over the limit",
                1_795_165_575_446_761_000
            ),
            (
                LogLevel::Error,
                "Payment provider timed out",
                1_795_165_575_446_854_000
            ),
        ]
    );
    let first = &records[0];
    assert_eq!(first.source, LogSource::Sentry);
    assert_eq!(
        first.trace_id.as_deref(),
        Some("54389bcc112e48989ca2ecdfe5f62b5c")
    );
    assert_eq!(first.span_id, None);
    assert_eq!(first.attributes["order.id"], "ord-1001");
    assert_eq!(first.attributes["amount"], 42.5);
    assert_eq!(first.attributes["items"], 3);
    assert_eq!(first.attributes["sentry.release"], "shop@2.4.0");
    assert_eq!(records[2].attributes["retry"], true);
    assert!(sentry::parse(b"{\"version\":2}", 1, 1).is_err());
}

#[test]
fn otlp_fixtures_json_and_proto_agree() {
    for (dir, encoding) in [
        ("otlp-json", otlp::Encoding::Json),
        ("otlp-proto", otlp::Encoding::Protobuf),
    ] {
        let first = otlp::parse(&fixture(&format!("{dir}/1.body")), encoding, 3, 7).unwrap();
        assert_eq!(first.len(), 1, "{dir}");
        let r = &first[0];
        assert_eq!(r.level, LogLevel::Info);
        assert_eq!(r.message, "GET /cart 200");
        assert_eq!(r.service.as_deref(), Some("checkout"));
        assert_eq!(
            r.trace_id.as_deref(),
            Some("5b8efff798038103d269b633813fc60c")
        );
        assert_eq!(r.span_id.as_deref(), Some("eee19b7ec3c1b174"));
        assert_eq!(
            r.attributes,
            attrs(json!({
                "http.status_code": 200,
                "otel.scope.name": "checkout.http",
                "resource.deployment.environment": "test",
                "user.id": "u-17"
            }))
        );
        let second = otlp::parse(&fixture(&format!("{dir}/2.body")), encoding, 3, 7).unwrap();
        let r = &second[0];
        assert_eq!(r.level, LogLevel::Error);
        assert_eq!(r.message, r#"{"error":"db timeout","ms":5000}"#);
        assert_eq!(r.trace_id, None);
        assert_eq!(r.attributes["tags"], json!(["db", "cart"]));
        assert_eq!(r.attributes["retry"], false);
    }
    let first = otlp::parse(&fixture("otlp-json/1.body"), otlp::Encoding::Json, 3, 7).unwrap();
    assert_eq!(first[0].timestamp, 1_795_165_575_662_000_000);
}

#[test]
fn otlp_json_accepts_int64_as_string() {
    let body = br#"{"resourceLogs":[{"scopeLogs":[{"logRecords":[{"timeUnixNano":"1795165575662000000","severityNumber":13,"body":{"stringValue":"slow"},"attributes":[{"key":"ms","value":{"intValue":"5000"}}]}]}]}]}"#;
    let records = otlp::parse(body, otlp::Encoding::Json, 1, 7).unwrap();
    assert_eq!(records[0].level, LogLevel::Warn);
    assert_eq!(records[0].attributes["ms"], 5000);
    assert_eq!(records[0].timestamp, 1_795_165_575_662_000_000);
    assert!(otlp::parse(b"not json", otlp::Encoding::Json, 1, 7).is_err());
    assert!(otlp::parse(b"\xff\xff\xff", otlp::Encoding::Protobuf, 1, 7).is_err());
}

/// Every request the server decodes must also have a price, because a
/// request without one is refused. These came from the fuzz targets.
#[test]
fn a_request_that_decodes_has_a_price() {
    // An unknown field 12 sent as an empty group, which prost skips.
    let group = b"cd";
    assert!(otlp::decode(group, otlp::Encoding::Protobuf).is_ok());
    assert!(otlp::cost(group, otlp::Encoding::Protobuf).is_ok());
    // A lone end of a group is broken either way.
    assert!(otlp::decode(b"d", otlp::Encoding::Protobuf).is_err());
    // JSON must be UTF-8, even inside a field that is not read.
    let latin = b"{\"resourceLogs\":[],\"x\":\"\xff\"}";
    assert!(otlp::decode(latin, otlp::Encoding::Json).is_err());
    // rmpv reads the unused marker 0xc1 as nil.
    let reserved = [0x81, 0xc1, 0x74];
    assert!(logtail::parse(&reserved, "application/msgpack", 1, 7).is_ok());
    assert!(logtail::cost(&reserved, "application/msgpack").is_ok());
}

#[test]
fn otlp_status_bodies() {
    assert_eq!(
        otlp::status(otlp::Encoding::Json, "bad"),
        br#"{"code":3,"message":"bad"}"#
    );
    assert_eq!(
        otlp::status(otlp::Encoding::Protobuf, "bad"),
        b"\x08\x03\x12\x03bad"
    );
    assert_eq!(otlp::success(otlp::Encoding::Json), b"{}");
    assert_eq!(otlp::success(otlp::Encoding::Protobuf), b"");
    assert_eq!(
        otlp::Encoding::from_content_type("application/json; charset=utf-8"),
        Some(otlp::Encoding::Json)
    );
    assert_eq!(otlp::Encoding::from_content_type("text/plain"), None);
}

#[test]
fn level_table() {
    use LogLevel::*;
    for (text, level) in [
        ("trace", Trace),
        ("DEBUG", Debug),
        ("info", Info),
        ("information", Info),
        ("notice", Info),
        ("log", Info),
        ("warn", Warn),
        ("Warning", Warn),
        ("error", Error),
        ("err", Error),
        ("fatal", Fatal),
        ("critical", Fatal),
        ("crit", Fatal),
        ("alert", Fatal),
        ("emerg", Fatal),
        ("emergency", Fatal),
        ("panic", Fatal),
    ] {
        assert_eq!(LogLevel::parse(text), Some(level), "{text}");
    }
    assert_eq!(LogLevel::parse("loud"), None);
    for (n, level) in [
        (10.0, Trace),
        (20.0, Debug),
        (30.0, Info),
        (40.0, Warn),
        (50.0, Error),
        (60.0, Fatal),
        (35.0, Warn),
    ] {
        assert_eq!(LogLevel::from_pino(n), level, "{n}");
    }
    for (n, level) in [
        (1, Trace),
        (4, Trace),
        (5, Debug),
        (8, Debug),
        (9, Info),
        (12, Info),
        (13, Warn),
        (16, Warn),
        (17, Error),
        (20, Error),
        (21, Fatal),
        (24, Fatal),
    ] {
        assert_eq!(LogLevel::from_severity(n, ""), level, "{n}");
    }
    assert_eq!(LogLevel::from_severity(0, "WARN"), Warn);
    assert_eq!(LogLevel::from_severity(0, "?"), Info);
    assert_eq!(LogLevel::from_value(Some(&json!(true))), Info);
    assert_eq!(LogLevel::from_value(None), Info);
}

#[test]
fn timestamp_forms() {
    for (v, ns) in [
        (json!(1_795_161_600), Some(1_795_161_600_000_000_000)),
        (json!(1_795_161_600.25), Some(1_795_161_600_250_000_000)),
        (json!(1_795_161_600_123i64), Some(1_795_161_600_123_000_000)),
        (
            json!(1_795_161_600_123_456i64),
            Some(1_795_161_600_123_456_000),
        ),
        (
            json!(1_795_161_600_123_456_789i64),
            Some(1_795_161_600_123_456_789),
        ),
        (
            json!("2026-11-20T08:00:00Z"),
            Some(1_795_161_600_000_000_000),
        ),
        (
            json!("2026-11-20T09:00:00.5+01:00"),
            Some(1_795_161_600_500_000_000),
        ),
        (
            json!("2026-11-20T08:00:00.001"),
            Some(1_795_161_600_001_000_000),
        ),
        (json!(-5), None),
        (json!("soon"), None),
        (json!(null), None),
        (json!({"s": 1}), None),
    ] {
        assert_eq!(timestamp(&v), ns, "{v}");
    }
}

#[test]
fn ids() {
    assert_eq!(
        hex_id("ABCDEF0123456789", 16).as_deref(),
        Some("abcdef0123456789")
    );
    assert_eq!(hex_id("0000000000000000", 16), None);
    assert_eq!(hex_id("abc", 16), None);
    assert_eq!(hex_bytes(&[0xee, 0x01], 4).as_deref(), Some("ee01"));
}

fn record(
    project_id: i64,
    timestamp: i64,
    observed_at: i64,
    level: LogLevel,
    message: &str,
) -> LogRecord {
    LogRecord {
        project_id,
        timestamp,
        observed_at,
        source: LogSource::Logtail,
        level,
        message: message.into(),
        service: Some("api".into()),
        trace_id: None,
        span_id: None,
        attributes: attrs(json!({"provider": "acme-pay", "http": {"status": 502}})),
    }
}

fn write(dir: &std::path::Path, retention_days: u32, batch: Vec<LogRecord>) {
    let writer =
        LogWriter::start(dir.to_path_buf(), retention_days, store::no_overrides()).unwrap();
    assert!(writer.queue.push(batch).is_ok());
    drop(writer.queue);
    writer.thread.join().unwrap();
}

fn query(since: i64, until: i64) -> LogQuery {
    LogQuery {
        project: None,
        since,
        until,
        min_level: None,
        text: None,
        limit: 100,
        skip: 0,
    }
}

const DAY: i64 = 86_400_000_000_000;

fn day_name(ns: i64) -> String {
    time::day_of(ns).to_string()
}

fn day_dirs(dir: &std::path::Path) -> Vec<String> {
    let mut days: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|name| !name.starts_with('.'))
        .collect();
    days.sort();
    days
}

fn four_records(now: i64) -> Vec<LogRecord> {
    vec![
        record(1, now - DAY, now, LogLevel::Info, "yesterday checkout ok"),
        record(
            1,
            now - 2,
            now,
            LogLevel::Error,
            "payment provider timed out",
        ),
        record(2, now - 1, now, LogLevel::Warn, "other project"),
        record(1, now - 40 * DAY, now, LogLevel::Debug, "clock skew"),
    ]
}

#[test]
fn store_files_records_by_day_and_expires_old_days() {
    let dir = scratch("logs");
    let now = now_ns();
    let old = now - 5 * DAY;
    write(
        &dir,
        30,
        vec![record(1, old, old, LogLevel::Info, "ancient")],
    );
    assert_eq!(day_dirs(&dir), [day_name(old)]);

    write(&dir, 2, four_records(now));
    let mut expected = vec![day_name(now - DAY), day_name(now)];
    expected.dedup();
    assert_eq!(
        day_dirs(&dir),
        expected,
        "old day expired, two live days remain"
    );
    assert_eq!(std::fs::read_dir(dir.join(".trash")).unwrap().count(), 0);

    let all = search(&dir, &query(now - 3 * DAY, now + 1)).unwrap();
    let messages: Vec<_> = all.iter().map(|h| h.message.as_str()).collect();
    assert_eq!(
        messages,
        [
            "yesterday checkout ok",
            "payment provider timed out",
            "other project",
            "clock skew"
        ],
        "oldest first"
    );
    assert_eq!(
        all[3].timestamp_ns, now,
        "a future timestamp falls back to observed"
    );
    assert_eq!(all[3].attributes[ORIGINAL_TIMESTAMP], now - 40 * DAY);
    assert_eq!(all[1].level, LogLevel::Error);
    assert_eq!(all[1].service.as_deref(), Some("api"));
    assert_eq!(all[1].attributes["http"]["status"], 502);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn search_filters_by_project_level_text_and_limit() {
    let dir = scratch("logs");
    let now = now_ns();
    write(&dir, 30, four_records(now));
    let messages = |q: LogQuery| -> Vec<String> {
        search(&dir, &q)
            .unwrap()
            .into_iter()
            .map(|h| h.message)
            .collect()
    };
    let window = query(now - 3 * DAY, now + 1);
    let cases: [(LogQuery, &[&str]); 6] = [
        (
            LogQuery {
                project: Some(2),
                ..window.clone()
            },
            &["other project"],
        ),
        (
            LogQuery {
                min_level: Some(LogLevel::Warn),
                ..window.clone()
            },
            &["payment provider timed out", "other project"],
        ),
        (
            LogQuery {
                text: Some("timed".into()),
                ..window.clone()
            },
            &["payment provider timed out"],
        ),
        (
            LogQuery {
                text: Some("attributes.provider:acme".into()),
                project: Some(2),
                ..window.clone()
            },
            &["other project"],
        ),
        (
            LogQuery {
                limit: 1,
                ..window.clone()
            },
            &["clock skew"],
        ),
        (query(now - 3 * DAY, now - DAY - 1), &[]),
    ];
    for (q, expected) in cases {
        assert_eq!(messages(q.clone()), expected, "{q:?}");
    }
    assert_eq!(
        search(&dir.join("missing"), &query(0, now)).unwrap().len(),
        0
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn search_passes_over_the_newest_hits_at_until_only() {
    let dir = scratch("logs");
    let now = now_ns();
    let records = ["a", "b", "c"]
        .map(|m| record(1, now, now, LogLevel::Info, m))
        .into_iter()
        .chain([record(1, now - 1, now, LogLevel::Info, "older")])
        .collect();
    write(&dir, 30, records);
    let page = |skip: usize| -> Vec<String> {
        let q = LogQuery {
            limit: 2,
            skip,
            ..query(now - DAY, now)
        };
        search(&dir, &q)
            .unwrap()
            .into_iter()
            .map(|h| h.message)
            .collect()
    };
    let (first, second) = (page(0), page(2));
    assert_eq!(second[0], "older", "oldest first");
    let all: std::collections::HashSet<_> = first.iter().chain(&second).collect();
    assert_eq!(all.len(), 4, "the second page repeats none of the first");
    assert_eq!(page(9), ["older"], "only hits at until are passed over");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn store_recovers_when_a_day_index_breaks() {
    let dir = scratch("logs");
    let now = now_ns();
    let day = dir.join(day_name(now));
    let batch = move |n: usize, message: &str| -> Vec<LogRecord> {
        (0..n)
            .map(|i| {
                record(
                    1,
                    now - i64::try_from(i).unwrap(),
                    now,
                    LogLevel::Info,
                    message,
                )
            })
            .collect()
    };
    let root = dir.clone();
    // A dead indexing worker blocks add_document forever, so the steps run
    // on a thread and the test waits for it with a deadline. The steps take
    // about 3 s alone and over 30 s in the full suite on a loaded machine.
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut state = State::new(root, 30, store::no_overrides());
        state.add(batch(10, "before")).unwrap();
        state.commit().unwrap();
        std::fs::remove_dir_all(&day).unwrap();
        let broken = state.add(batch(10, "lost")).and_then(|()| state.commit());
        for _ in 0..3 {
            state.add(batch(10_000, "after")).unwrap();
        }
        state.commit().unwrap();
        state.close_all().unwrap();
        done.send(broken.is_err()).unwrap();
    });
    assert_eq!(
        finished.recv_timeout(std::time::Duration::from_secs(120)),
        Ok(true),
        "the write into the deleted index failed and the next writes reopened it"
    );
    let hits = search(
        &dir,
        &LogQuery {
            text: Some("after".into()),
            limit: 50_000,
            skip: 0,
            ..query(now - 1_000_000_000, now + 1)
        },
    )
    .unwrap();
    assert_eq!(hits.len(), 30_000);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn long_messages_are_cut_at_64_kib_on_a_character_boundary() {
    // The two bytes of "é" straddle the 64 KiB mark, so the cut lands before it.
    let long = serde_json::to_string(&format!("{}é tail", "a".repeat(65_535))).unwrap();
    let logtail = format!(r#"{{"message":{long}}}"#);
    let sentry = format!(r#"{{"items":[{{"body":{long}}}]}}"#);
    let otlp = format!(
        r#"{{"resourceLogs":[{{"scopeLogs":[{{"logRecords":[{{"body":{{"stringValue":{long}}}}}]}}]}}]}}"#
    );
    for records in [
        logtail::parse(logtail.as_bytes(), "application/json", 1, 7).unwrap(),
        sentry::parse(sentry.as_bytes(), 1, 7).unwrap(),
        otlp::parse(otlp.as_bytes(), otlp::Encoding::Json, 1, 7).unwrap(),
    ] {
        assert_eq!(records[0].message.len(), 65_535);
        assert_eq!(
            records[0].attributes.get("davy.truncated"),
            Some(&json!(true))
        );
    }
    let short = logtail::parse(br#"{"message":"short"}"#, "application/json", 1, 7).unwrap();
    assert_eq!(short[0].attributes.get("davy.truncated"), None);
}

#[test]
fn a_message_is_priced_by_what_it_holds() {
    let cut = format!(r#"{{"message":"{}"}}"#, "a".repeat(1 << 20));
    let printed = format!(r#"{{"message":{{"a":"{}"}}}}"#, "b".repeat(40_000));
    for body in [cut, printed] {
        let records = logtail::parse(body.as_bytes(), "application/json", 1, 7).unwrap();
        let message = &records[0].message;
        assert!(crate::heap::of_record(&records[0]) >= message.capacity());
        assert_eq!(message.capacity(), message.len());
    }
}

#[test]
fn the_histogram_counts_across_two_day_indexes_into_round_buckets() {
    let dir = scratch("logs");
    let now = now_ns();
    let midnight = now.div_euclid(DAY) * DAY - DAY;
    let (s, m) = (1_000_000_000, 60_000_000_000);
    let at = |offset: i64, level| record(1, midnight + offset, now, level, "tick");
    write(
        &dir,
        30,
        vec![
            at(-90 * s, LogLevel::Info),
            at(-30 * s, LogLevel::Error),
            at(-1, LogLevel::Info),
            at(0, LogLevel::Info),
            at(s, LogLevel::Error),
            at(59 * s, LogLevel::Info),
            at(61 * s, LogLevel::Info),
            at(3 * m, LogLevel::Info),
        ],
    );
    assert_eq!(day_dirs(&dir), [day_name(midnight - 1), day_name(midnight)]);
    let window = query(midnight - 2 * m, midnight + 2 * m - 1);
    let h = histogram(&dir, &window).unwrap();
    assert_eq!(
        (h.from, h.step, h.counts.len()),
        (midnight - 2 * m, 2 * s, 120)
    );
    let nonzero = |counts: &[u64]| -> Vec<(usize, u64)> {
        counts
            .iter()
            .enumerate()
            .filter(|(_, n)| **n > 0)
            .map(|(i, n)| (i, *n))
            .collect()
    };
    assert_eq!(
        nonzero(&h.counts),
        [(15, 1), (45, 1), (59, 1), (60, 2), (89, 1), (90, 1)],
        "2 s buckets from two minutes before midnight, the log at 3 min outside"
    );
    let errors = LogQuery {
        min_level: Some(LogLevel::Error),
        ..window
    };
    let h = histogram(&dir, &errors).unwrap();
    assert_eq!(nonzero(&h.counts), [(45, 1), (60, 1)]);
    let empty = histogram(&dir.join("missing"), &query(0, 10)).unwrap();
    assert_eq!(
        (empty.from, empty.step, empty.counts),
        (0, 1_000_000, vec![0])
    );
    std::fs::remove_dir_all(&dir).unwrap();
}
