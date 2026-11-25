//! The `usage` tool.

use super::*;

/// The keys of the usage tool's `disk`.
const KEYS: [&str; 12] = [
    "basis",
    "davy",
    "default_log_days",
    "disk",
    "estimate",
    "forecast",
    "growth_per_day",
    "longest_log_days",
    "projects",
    "shortest_log_days",
    "what_if",
    "window",
];

const DISK_KEYS: [&str; 11] = [
    "beyond_10_years",
    "days_left",
    "free",
    "full_on",
    "headroom",
    "headroom_reason",
    "partial",
    "steady_state",
    "total",
    "used",
    "worst_case",
];

/// Stored logs by index day: those of `yesterday` are in a 2-day window,
/// those of three days ago are not.
fn write_stored(c: &rusqlite::Connection, yesterday: jiff::civil::Date) {
    let stored = |day, records, bytes| logs::store::log_days::Row {
        day,
        project: 1,
        count: usage::Count { records, bytes },
    };
    let earlier = yesterday.checked_sub(jiff::ToSpan::days(2)).unwrap();
    let flush = logs::store::log_days::Flush {
        start: None,
        segments: vec![logs::store::log_days::Segment {
            rows: vec![
                stored(earlier, 1, 1),
                stored(yesterday, 19_000, 3_900_000_000),
            ],
            marks: Vec::new(),
        }],
    };
    usage::stored::write(c, &flush).unwrap();
}

/// What [`write_stored`] wrote for `yesterday`, as `usage` shows it.
fn assert_stored(stored: &Value) {
    let keys: Vec<_> = stored.as_object().unwrap().keys().cloned().collect();
    assert_eq!(
        keys,
        ["bytes", "bytes_per_day", "records", "records_per_day"]
    );
    assert_eq!(
        (&stored["records"], &stored["bytes"]),
        (&json!(19_000), &json!("3.9 GB"))
    );
}

#[tokio::test]
async fn usage_shows_the_counts_and_the_forecast_with_human_sizes() {
    let s = server();
    let yesterday = time::day_of(time::now_ns())
        .checked_sub(jiff::ToSpan::days(1))
        .unwrap();
    let c = store::open(&s.data).unwrap();
    let row = |signal, records, bytes| usage::Row {
        day: yesterday,
        project: 1,
        signal,
        count: usage::Count { records, bytes },
    };
    usage::write(
        &c,
        &[
            row(usage::Signal::Logs, 20_000, 4_100_000_000),
            row(usage::Signal::Errors, 7, 21_000),
        ],
    )
    .unwrap();
    write_stored(&c, yesterday);
    let day = s.data.join("logs").join(yesterday.to_string());
    std::fs::create_dir_all(&day).unwrap();
    std::fs::write(day.join("segment"), [1_u8; 4096]).unwrap();
    let (text, error) = tool(
        &s.app,
        "usage",
        json!({"since": "2d", "what_if": {"logs_bytes_per_day": 1_000_000}}),
    )
    .await;
    assert!(!error, "{text}");
    let v: Value = serde_json::from_str(&text).unwrap();
    let keys = |v: &Value| v.as_object().unwrap().keys().cloned().collect::<Vec<_>>();
    assert_eq!(keys(&v), KEYS);
    assert_eq!(keys(&v["disk"]), DISK_KEYS);
    assert_eq!(v["disk"]["partial"], Value::Null);
    assert_eq!(v["growth_per_day"], "1.0 MB");
    // A what-if of the bytes per day states the growth: it rests on
    // nothing, and gives no raw bytes per project to estimate from.
    assert_eq!(v["basis"], Value::Null);
    assert_eq!(v["estimate"], Value::Null);
    assert_eq!(v["disk"]["beyond_10_years"], false);
    assert!(v["disk"]["total"].as_str().unwrap().ends_with('B'), "{v}");
    let api = &v["projects"][0];
    // The test server's project keeps the default retention.
    let days = (&v["longest_log_days"], &v["default_log_days"]);
    assert_eq!(days, (&json!(30), &json!(30)));
    assert_eq!(
        keys(api),
        [
            "errors_days",
            "errors_discarded",
            "logs_days",
            "logs_stored",
            "project",
            "signals"
        ]
    );
    assert_stored(&api["logs_stored"]);
    assert_eq!(
        (&api["project"], &api["logs_days"]),
        (&json!("api"), &json!(30))
    );
    assert_eq!(api["signals"]["logs"]["records"], 20_000);
    assert_eq!(api["signals"]["logs"]["bytes"], "4.1 GB");
    assert_eq!(api["signals"]["errors"]["bytes"], "21.0 KB");
    assert_eq!(
        keys(&api["signals"]["errors"]),
        ["bytes", "bytes_per_day", "records", "records_per_day"]
    );
}

#[tokio::test]
async fn usage_refuses_a_what_if_or_since_it_cannot_answer() {
    let s = server();
    let unknown = tool(&s.app, "usage", json!({"project": "web"})).await;
    assert_eq!(unknown, ("unknown project \"web\"".to_owned(), true));
    let long = tool(&s.app, "usage", json!({"since": "91d"})).await;
    assert_eq!(long, ("since reaches back at most 90d".to_owned(), true));
    let one = tool(
        &s.app,
        "usage",
        json!({"what_if": {"project": "api", "logs_bytes_per_day": 1}}),
    )
    .await;
    assert_eq!(
        one,
        ("logs_bytes_per_day is for all projects together: logs of every project share one index per day, so Davy cannot tell one project's bytes on disk".to_owned(), true)
    );
    let ghost = tool(
        &s.app,
        "usage",
        json!({"what_if": {"project": "web", "logs_days": 7}}),
    )
    .await;
    assert_eq!(ghost, ("unknown project \"web\"".to_owned(), true));
    let nothing = tool(&s.app, "usage", json!({"what_if": {"project": "api"}})).await;
    assert_eq!(
        nothing,
        (
            "a what-if for project api needs logs_days, the days of logs it would keep".to_owned(),
            true
        )
    );
    let zero = tool(&s.app, "usage", json!({"what_if": {"logs_days": 0}})).await;
    assert_eq!(zero, ("logs_days must be 1 to 36500".to_owned(), true));
    let later = time::day_of(time::now_ns())
        .checked_add(jiff::ToSpan::days(2))
        .unwrap();
    let since = format!("{later}T00:00:00Z");
    let future = tool(&s.app, "usage", json!({ "since": since })).await;
    assert_eq!(future, ("since is in the future".to_owned(), true));
}
