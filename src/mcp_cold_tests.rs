//! The log tools over cold days.

use super::*;
use crate::logs::{LogRecord, LogSource, store::cold::cold_day};

#[tokio::test]
async fn a_search_that_reads_cold_days_says_so_in_a_second_text() {
    let s = server();
    let now = time::now_ns();
    let day = time::day_of(now - 10 * time::DAY_NS);
    let start = time::midnight(day);
    let log = |i: i64| LogRecord {
        project_id: 1,
        timestamp: start + i * 1_000_000_000,
        observed_at: now,
        source: LogSource::Logtail,
        level: if i % 3 == 0 {
            LogLevel::Error
        } else {
            LogLevel::Info
        },
        level_unknown: false,
        message: format!("job {i} timeout"),
        service: None,
        trace_id: None,
        span_id: None,
        attributes: serde_json::Map::new(),
    };
    cold_day(&logs::dir(&s.data), (0..30).map(log).collect()).unwrap();
    let window = json!({"since": time::rfc3339(start), "until": time::rfc3339(start + time::DAY_NS - 1),
                        "query": "timeout", "level": "error", "limit": 2});

    let (status, body) = post(
        &s.app,
        "localhost",
        Some(SECRET),
        call("search_logs", &window),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let reply: Value = serde_json::from_str(&body).unwrap();
    let content = reply["result"]["content"].as_array().unwrap();
    assert_eq!(content.len(), 2, "{body}");
    let hits: Value = serde_json::from_str(content[0]["text"].as_str().unwrap()).unwrap();
    let messages: Vec<_> = hits
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["message"].clone())
        .collect();
    assert_eq!(messages, [json!("job 24 timeout"), json!("job 27 timeout")]);
    let note: Value = serde_json::from_str(content[1]["text"].as_str().unwrap()).unwrap();
    let cold = &note["cold"];
    assert_eq!(cold["cold_from"], time::rfc3339(start));
    assert_eq!(cold["cold_to"], time::rfc3339(start + time::DAY_NS - 1));
    assert_eq!(cold["rows_read"], 30);
    assert!(cold.get("unscanned_to").is_none(), "{note}");

    let (text, is_error) = tool(&s.app, "count_logs", window).await;
    assert!(!is_error, "{text}");
    let counts: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(counts["total"], 10);
    assert_eq!(counts["cold"]["cold_from"], time::rfc3339(start));

    // A window of hot days only reads no cold file and says nothing of it.
    let hot = json!({"since": time::rfc3339(now - time::DAY_NS)});
    let (status, body) = post(&s.app, "localhost", Some(SECRET), call("search_logs", &hot)).await;
    assert_eq!(status, 200, "{body}");
    let reply: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        reply["result"]["content"].as_array().unwrap().len(),
        1,
        "{body}"
    );
    let (text, _) = tool(&s.app, "count_logs", hot).await;
    assert!(
        serde_json::from_str::<Value>(&text)
            .unwrap()
            .get("cold")
            .is_none(),
        "{text}"
    );
}
