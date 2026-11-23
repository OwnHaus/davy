use super::{LogQuery, LogWriter, schema, search};
use crate::logs::{LogLevel, LogRecord, LogSource};
use crate::test_support::scratch;
use crate::time::now_ns;
use serde_json::json;

fn record(timestamp: i64, message: &str, attributes: serde_json::Value) -> LogRecord {
    LogRecord {
        project_id: 1,
        timestamp,
        observed_at: timestamp,
        source: LogSource::Logtail,
        level: LogLevel::Info,
        message: message.into(),
        service: None,
        trace_id: None,
        span_id: None,
        attributes: match attributes {
            serde_json::Value::Object(m) => m,
            _ => unreachable!(),
        },
    }
}

fn write(dir: &std::path::Path, batch: Vec<LogRecord>) {
    let writer = LogWriter::start(dir.to_path_buf(), 30).unwrap();
    assert!(writer.queue.push(batch).is_ok());
    drop(writer.queue);
    writer.thread.join().unwrap();
}

fn messages(dir: &std::path::Path, now: i64, text: &str) -> Vec<String> {
    let q = LogQuery {
        project: None,
        since: now - 1000,
        until: now + 1000,
        min_level: None,
        text: Some(text.into()),
        limit: 10,
        skip: 0,
    };
    search(dir, &q)
        .unwrap()
        .into_iter()
        .map(|h| h.message)
        .collect()
}

#[test]
fn a_bare_word_finds_a_value_at_any_depth_of_the_attributes() {
    let dir = scratch("values");
    let now = now_ns();
    write(
        &dir,
        vec![
            record(
                now - 2,
                "request completed",
                json!({"req": {"id": "r-3fa9c2d1", "method": "POST"}, "hostname": "api-7d9f-0b1c"}),
            ),
            record(
                now - 1,
                "cache refreshed",
                json!({"user": {"id": 4211}, "tags": ["blue", "canary"]}),
            ),
        ],
    );
    let cases: [(&str, &[&str]); 8] = [
        ("3fa9c2d1", &["request completed"]),
        ("r-3fa9c2d1", &["request completed"]),
        ("0b1c", &["request completed"]),
        ("4211", &["cache refreshed"]),
        ("canary", &["cache refreshed"]),
        ("\"api 7d9f\"", &["request completed"]),
        ("attributes.req.method:post", &["request completed"]),
        ("method", &[]),
    ];
    for (text, expected) in cases {
        assert_eq!(messages(&dir, now, text), expected, "{text}");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_day_written_before_the_values_field_still_takes_writes_and_queries() {
    let dir = scratch("values-old");
    let now = now_ns();
    let mut old = tantivy::schema::Schema::builder();
    for (_, entry) in schema().0.fields().filter(|(_, e)| e.name() != "values") {
        old.add_field(entry.clone());
    }
    let day = dir.join(crate::time::day_of(now).to_string());
    std::fs::create_dir_all(&day).unwrap();
    tantivy::Index::create_in_dir(&day, old.build()).unwrap();
    write(
        &dir,
        vec![record(
            now,
            "request completed",
            json!({"req": {"id": "r-3fa9c2d1"}}),
        )],
    );
    assert_eq!(
        messages(&dir, now, "attributes.req.id:3fa9c2d1"),
        ["request completed"]
    );
    assert_eq!(messages(&dir, now, "completed"), ["request completed"]);
    assert_eq!(messages(&dir, now, "3fa9c2d1"), Vec::<String>::new());
    std::fs::remove_dir_all(&dir).unwrap();
}
