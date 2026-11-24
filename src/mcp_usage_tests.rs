//! The `usage` tool.

use super::*;

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
    assert_eq!(
        keys(&v),
        [
            "davy",
            "disk",
            "forecast",
            "growth_per_day",
            "longest_log_days",
            "projects",
            "what_if",
            "window"
        ]
    );
    assert_eq!(
        keys(&v["disk"]),
        [
            "days_left",
            "free",
            "full_on",
            "headroom",
            "headroom_reason",
            "steady_state",
            "total",
            "used"
        ]
    );
    assert_eq!(v["growth_per_day"], "1.0 MB");
    assert!(v["disk"]["total"].as_str().unwrap().ends_with('B'), "{v}");
    let api = &v["projects"][0];
    assert_eq!(v["longest_log_days"], 30);
    assert_eq!(
        keys(api),
        ["errors_days", "logs_days", "project", "signals"]
    );
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
    let unknown = tool(&s.app, "usage", json!({"project": "web"})).await;
    assert_eq!(unknown, ("unknown project \"web\"".to_owned(), true));
    let long = tool(&s.app, "usage", json!({"since": "91d"})).await;
    assert_eq!(long, ("since reaches back at most 90d".to_owned(), true));
}

#[tokio::test]
async fn usage_refuses_a_what_if_or_since_it_cannot_answer() {
    let s = server();
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
    let zero = tool(&s.app, "usage", json!({"what_if": {"logs_days": 0}})).await;
    assert_eq!(zero, ("logs_days must be 1 to 36500".to_owned(), true));
    let later = time::day_of(time::now_ns())
        .checked_add(jiff::ToSpan::days(2))
        .unwrap();
    let since = format!("{later}T00:00:00Z");
    let future = tool(&s.app, "usage", json!({ "since": since })).await;
    assert_eq!(future, ("since is in the future".to_owned(), true));
}
