use super::*;
use crate::logs::ORIGINAL_TIMESTAMP;
use serde_json::json;

/// The JSON example of Vercel's log drain reference, as printed there
/// (<https://vercel.com/docs/drains/reference/logs#json>): a build log and
/// a function log with a proxy object. Not a capture of a real drain.
const DOCS: &str = r#"[
  { "id": "1573817187330377061717300000", "deploymentId": "dpl_233NRGRjVZX1caZrXWtz5g1TAksD", "source": "build", "host": "my-app-abc123.vercel.app", "timestamp": 1573817187330, "projectId": "gdufoJxB6b9b1fEqr1jUtFkyavUU", "level": "info", "message": "Build completed successfully", "buildId": "bld_cotnkcr76", "type": "stdout", "projectName": "my-app" },
  { "id": "1573817250283254651097202070", "deploymentId": "dpl_233NRGRjVZX1caZrXWtz5g1TAksD", "source": "lambda", "host": "my-app-abc123.vercel.app", "timestamp": 1573817250283, "projectId": "gdufoJxB6b9b1fEqr1jUtFkyavUU", "level": "info", "message": "API request processed", "entrypoint": "api/index.js", "requestId": "643af4e3-975a-4cc7-9e7a-1eda11539d90", "statusCode": 200, "path": "/api/users", "executionRegion": "sfo1", "environment": "production", "traceId": "1b02cd14bb8642fd092bc23f54c7ffcd", "spanId": "f24e8631bd11faa7", "proxy": { "timestamp": 1573817250172, "method": "GET", "host": "my-app.vercel.app", "path": "/api/users?page=1", "userAgent": ["Mozilla/5.0..."], "referer": "https://my-app.vercel.app", "region": "sfo1", "statusCode": 200, "clientIp": "120.75.16.101", "scheme": "https", "vercelCache": "MISS" } }
]"#;

/// The NDJSON example of the same page, the same two records one per line.
const DOCS_NDJSON: &str = r#"{"id": "1573817187330377061717300000","deploymentId": "dpl_233NRGRjVZX1caZrXWtz5g1TAksD","source": "build","host": "my-app-abc123.vercel.app","timestamp": 1573817187330,"projectId": "gdufoJxB6b9b1fEqr1jUtFkyavUU","level": "info","message": "Build completed successfully","buildId": "bld_cotnkcr76","type": "stdout","projectName": "my-app"}
{"id": "1573817250283254651097202070","deploymentId": "dpl_233NRGRjVZX1caZrXWtz5g1TAksD","source": "lambda","host": "my-app-abc123.vercel.app","timestamp": 1573817250283,"projectId": "gdufoJxB6b9b1fEqr1jUtFkyavUU","level": "info","message": "API request processed","entrypoint": "api/index.js","requestId": "643af4e3-975a-4cc7-9e7a-1eda11539d90","statusCode": 200,"path": "/api/users","executionRegion": "sfo1","environment": "production","traceId": "1b02cd14bb8642fd092bc23f54c7ffcd","spanId": "f24e8631bd11faa7","proxy": {"timestamp": 1573817250172,"method": "GET","host": "my-app.vercel.app","path": "/api/users?page=1","userAgent": ["Mozilla/5.0..."],"referer": "https://my-app.vercel.app","region": "sfo1","statusCode": 200,"clientIp": "120.75.16.101","scheme": "https","vercelCache": "MISS"}}
"#;

const OBSERVED: i64 = 1_795_161_600_000_000_000;

fn object(v: Value) -> Map<String, Value> {
    let Value::Object(m) = v else {
        panic!("not an object: {v}")
    };
    m
}

fn expected() -> Vec<LogRecord> {
    let build = LogRecord {
        project_id: 7,
        timestamp: 1_573_817_187_330_000_000,
        observed_at: OBSERVED,
        source: LogSource::Vercel,
        level: LogLevel::Info,
        level_unknown: false,
        message: "Build completed successfully".into(),
        service: Some("my-app".into()),
        trace_id: None,
        span_id: None,
        attributes: object(json!({
            "id": "1573817187330377061717300000",
            "deploymentId": "dpl_233NRGRjVZX1caZrXWtz5g1TAksD",
            "source": "build",
            "host": "my-app-abc123.vercel.app",
            "projectId": "gdufoJxB6b9b1fEqr1jUtFkyavUU",
            "buildId": "bld_cotnkcr76",
            "type": "stdout",
        })),
    };
    let lambda = LogRecord {
        project_id: 7,
        timestamp: 1_573_817_250_283_000_000,
        observed_at: OBSERVED,
        source: LogSource::Vercel,
        level: LogLevel::Info,
        level_unknown: false,
        message: "API request processed".into(),
        service: Some("gdufoJxB6b9b1fEqr1jUtFkyavUU".into()),
        trace_id: Some("1b02cd14bb8642fd092bc23f54c7ffcd".into()),
        span_id: Some("f24e8631bd11faa7".into()),
        attributes: object(json!({
            "id": "1573817250283254651097202070",
            "deploymentId": "dpl_233NRGRjVZX1caZrXWtz5g1TAksD",
            "source": "lambda",
            "host": "my-app-abc123.vercel.app",
            "projectId": "gdufoJxB6b9b1fEqr1jUtFkyavUU",
            "entrypoint": "api/index.js",
            "requestId": "643af4e3-975a-4cc7-9e7a-1eda11539d90",
            "statusCode": 200,
            "path": "/api/users",
            "executionRegion": "sfo1",
            "environment": "production",
            "proxy": {
                "timestamp": 1_573_817_250_172_i64,
                "method": "GET",
                "host": "my-app.vercel.app",
                "path": "/api/users?page=1",
                "userAgent": ["Mozilla/5.0..."],
                "referer": "https://my-app.vercel.app",
                "region": "sfo1",
                "statusCode": 200,
                "clientIp": "120.75.16.101",
                "scheme": "https",
                "vercelCache": "MISS",
            },
        })),
    };
    vec![build, lambda]
}

#[test]
fn the_documented_json_example_maps_field_by_field() {
    assert_eq!(parse(DOCS.as_bytes(), 7, OBSERVED).unwrap(), expected());
}

#[test]
fn the_documented_ndjson_example_maps_like_the_json_one() {
    assert_eq!(
        parse(DOCS_NDJSON.as_bytes(), 7, OBSERVED).unwrap(),
        expected()
    );
}

#[test]
fn one_record_on_its_own_is_a_batch_of_one() {
    let one = r#"{"timestamp": 1573817187330, "projectId": "p", "message": "m"}"#;
    let records = parse(one.as_bytes(), 7, OBSERVED).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].message, "m");
    assert_eq!(records[0].service.as_deref(), Some("p"));
}

#[test]
fn each_vercel_level_maps_to_its_own() {
    let body = r#"[{"level":"info"},{"level":"warning"},{"level":"error"},{"level":"fatal"},{}]"#;
    let levels: Vec<_> = parse(body.as_bytes(), 7, OBSERVED)
        .unwrap()
        .into_iter()
        .map(|r| r.level)
        .collect();
    assert_eq!(
        levels,
        [
            LogLevel::Info,
            LogLevel::Warn,
            LogLevel::Error,
            LogLevel::Fatal,
            LogLevel::Info
        ]
    );
}

#[test]
fn the_older_trace_keys_are_stored_once() {
    let both = r#"{"traceId":"1B02CD14BB8642FD092BC23F54C7FFCD","trace.id":"1b02cd14bb8642fd092bc23f54c7ffcd","spanId":"f24e8631bd11faa7","span.id":"f24e8631bd11faa7"}"#;
    let r = &parse(both.as_bytes(), 7, OBSERVED).unwrap()[0];
    assert_eq!(
        r.trace_id.as_deref(),
        Some("1b02cd14bb8642fd092bc23f54c7ffcd")
    );
    assert_eq!(r.span_id.as_deref(), Some("f24e8631bd11faa7"));
    assert_eq!(r.attributes, Map::new());

    let older = r#"{"trace.id":"1b02cd14bb8642fd092bc23f54c7ffcd","span.id":"zz"}"#;
    let r = &parse(older.as_bytes(), 7, OBSERVED).unwrap()[0];
    assert_eq!(
        r.trace_id.as_deref(),
        Some("1b02cd14bb8642fd092bc23f54c7ffcd")
    );
    assert_eq!(r.span_id, None);
    assert_eq!(r.attributes, object(json!({"span.id": "zz"})));
}

#[test]
fn an_empty_project_name_falls_back_to_the_project_id() {
    let body = r#"{"projectName":"","projectId":"prj_1"}"#;
    let r = &parse(body.as_bytes(), 7, OBSERVED).unwrap()[0];
    assert_eq!(r.service.as_deref(), Some("prj_1"));
    assert_eq!(r.attributes, object(json!({"projectId": "prj_1"})));
}

#[test]
fn a_timestamp_that_is_not_a_time_is_kept_and_replaced() {
    let body = r#"{"timestamp":"soon","message":"m"}"#;
    let r = &parse(body.as_bytes(), 7, OBSERVED).unwrap()[0];
    assert_eq!(r.timestamp, OBSERVED);
    assert_eq!(r.attributes[ORIGINAL_TIMESTAMP], json!("soon"));
}

#[test]
fn a_record_that_is_not_an_object_is_skipped() {
    let body = r#"[{"message":"kept"}, 5, "x", null]"#;
    let records = parse(body.as_bytes(), 7, OBSERVED).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].message, "kept");
}

#[test]
fn a_body_that_is_not_json_is_refused_by_the_price_and_the_parser() {
    for body in ["{\"message\":", "not json", "[1,2", "{}\n{"] {
        assert!(parse(body.as_bytes(), 7, OBSERVED).is_err(), "{body}");
        assert!(cost(body.as_bytes()).is_err(), "{body}");
    }
    assert_eq!(
        parse(b"5", 7, OBSERVED).unwrap_err(),
        "expected a log record or an array of records"
    );
}

#[test]
fn an_empty_body_is_an_empty_batch() {
    assert_eq!(parse(b"", 7, OBSERVED).unwrap(), Vec::new());
    assert_eq!(parse(b"\n\n", 7, OBSERVED).unwrap(), Vec::new());
}

#[test]
fn the_price_covers_the_tree_and_the_records() {
    let tree = heap::json(DOCS.as_bytes(), RECORD).unwrap();
    assert_eq!(cost(DOCS.as_bytes()).unwrap(), 2 * tree);
    assert!(cost(DOCS_NDJSON.as_bytes()).unwrap() > 0);
}
