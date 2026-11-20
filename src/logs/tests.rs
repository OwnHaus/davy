use super::store::{LogQuery, LogWriter, search};
use super::*;
use crate::time::{self, now_ns};
use serde_json::json;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/e2e/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

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
fn a_msgpack_body_with_bytes_after_its_value_is_refused() {
    let mut body = Vec::new();
    let record = rmpv::Value::Map(vec![("message".into(), "x".into())]);
    rmpv::encode::write_value(&mut body, &record).unwrap();
    assert_eq!(
        logtail::parse(&body, "application/msgpack", 1, 5).unwrap()[0].message,
        "x"
    );
    body.push(0xc0);
    assert_eq!(
        logtail::parse(&body, "application/msgpack", 1, 5).unwrap_err(),
        "bytes after the msgpack value"
    );
}

#[test]
fn a_msgpack_timestamp_needs_fewer_than_1e9_nanoseconds() {
    let secs: u64 = 1_795_161_600;
    let packed = |nanos: u64| ((nanos << 34) | secs).to_be_bytes();
    let wide = |nanos: u32| {
        [
            nanos.to_be_bytes().as_slice(),
            &1_795_161_600i64.to_be_bytes(),
        ]
        .concat()
    };
    assert_eq!(
        logtail::msgpack_timestamp(&packed(999_999_999)),
        Some(1_795_161_600_999_999_999)
    );
    assert_eq!(logtail::msgpack_timestamp(&packed(1_000_000_000)), None);
    assert_eq!(
        logtail::msgpack_timestamp(&wide(999_999_999)),
        Some(1_795_161_600_999_999_999)
    );
    assert_eq!(logtail::msgpack_timestamp(&wide(1_000_000_000)), None);
}

#[test]
fn msgpack_timestamp_ext() {
    assert_eq!(
        logtail::msgpack_timestamp(&1_795_161_600u32.to_be_bytes()),
        Some(1_795_161_600_000_000_000)
    );
    let secs: u64 = 1_795_161_600;
    let packed = (500 << 34) | secs;
    assert_eq!(
        logtail::msgpack_timestamp(&packed.to_be_bytes()),
        Some(1_795_161_600_000_000_500)
    );
    let mut wide = 7u32.to_be_bytes().to_vec();
    wide.extend(1_795_161_600i64.to_be_bytes());
    assert_eq!(
        logtail::msgpack_timestamp(&wide),
        Some(1_795_161_600_000_000_007)
    );
    assert_eq!(logtail::msgpack_timestamp(&[0; 5]), None);

    let mut body = Vec::new();
    rmpv::encode::write_value(
        &mut body,
        &rmpv::Value::Map(vec![
            ("message".into(), "ext".into()),
            (
                "dt".into(),
                rmpv::Value::Ext(-1, 1_795_161_600u32.to_be_bytes().to_vec()),
            ),
        ]),
    )
    .unwrap();
    let records = logtail::parse(&body, "application/msgpack", 1, 5).unwrap();
    assert_eq!(records[0].timestamp, 1_795_161_600_000_000_000);
}

#[test]
fn sentry_log_fixture() {
    let envelope =
        crate::envelope::Envelope::parse(&fixture("sentry-node-10-logs/1.body")).unwrap();
    assert_eq!(envelope.items.len(), 1);
    assert_eq!(envelope.items[0].kind, crate::envelope::ItemKind::Log);
    let records = sentry::parse(&envelope.items[0].payload, 2, 7).unwrap();
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
    let writer = LogWriter::start(dir.to_path_buf(), retention_days).unwrap();
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
    }
}

const DAY: i64 = 86_400_000_000_000;

fn scratch_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("target/test-logs-{}", uuid::Uuid::new_v4()))
}

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
    let dir = scratch_dir();
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
            "clock skew",
            "other project",
            "payment provider timed out",
            "yesterday checkout ok"
        ]
    );
    assert_eq!(
        all[0].timestamp_ns, now,
        "a future timestamp falls back to observed"
    );
    assert_eq!(all[0].attributes[ORIGINAL_TIMESTAMP], now - 40 * DAY);
    assert_eq!(all[2].level, LogLevel::Error);
    assert_eq!(all[2].service.as_deref(), Some("api"));
    assert_eq!(all[2].attributes["http"]["status"], 502);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn search_filters_by_project_level_text_and_limit() {
    let dir = scratch_dir();
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
            &["other project", "payment provider timed out"],
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
fn store_recovers_when_a_day_index_breaks() {
    let dir = scratch_dir();
    let now = now_ns();
    let day = dir.join(day_name(now));
    let batch = |n: usize, message: &str| -> Vec<LogRecord> {
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
    let writer = LogWriter::start(dir.clone(), 30).unwrap();
    assert!(writer.queue.push(batch(10, "before")).is_ok());
    while !day.join("meta.json").exists() {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    std::thread::sleep(std::time::Duration::from_millis(1500));
    std::fs::remove_dir_all(&day).unwrap();
    assert!(writer.queue.push(batch(10, "lost")).is_ok());
    std::thread::sleep(std::time::Duration::from_millis(1500));
    for _ in 0..3 {
        while writer.queue.push(batch(10_000, "after")).is_err() {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
    drop(writer.queue);
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || done.send(writer.thread.join().is_ok()));
    assert_eq!(
        finished.recv_timeout(std::time::Duration::from_secs(30)),
        Ok(true),
        "writer shut down instead of blocking on a dead indexing worker"
    );
    let hits = search(
        &dir,
        &LogQuery {
            text: Some("after".into()),
            limit: 50_000,
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
fn the_log_queue_is_full_at_its_row_or_byte_limit() {
    use super::store::{LogQueue, QueueError};
    let big = || vec![record(1, 1, 1, LogLevel::Info, &"x".repeat(600_000))];
    let (queue, inbox) = LogQueue::bounded(1000, 1_000_000);
    assert!(queue.push(big()).is_ok());
    assert!(matches!(queue.push(big()), Err(QueueError::Full)));
    assert_eq!(inbox.try_recv().map(|b| b.len()), Some(1));
    assert!(queue.push(big()).is_ok(), "a drained batch frees its bytes");

    let small = || vec![record(1, 1, 1, LogLevel::Info, "x")];
    let (queue, _inbox) = LogQueue::bounded(2, 1_000_000);
    assert!(queue.push(small()).is_ok());
    assert!(queue.push(small()).is_ok());
    assert!(matches!(queue.push(small()), Err(QueueError::Full)));
}

#[test]
fn log_retention_is_1_to_36500_days() {
    let dir = scratch_dir();
    for days in [0, 36_501, u32::MAX] {
        assert!(LogWriter::start(dir.clone(), days).is_err(), "{days}");
    }
    let writer = LogWriter::start(dir, 36_500).unwrap();
    drop(writer.queue);
    writer.thread.join().unwrap();
}
