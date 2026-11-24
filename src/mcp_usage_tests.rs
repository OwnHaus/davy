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
    assert_eq!(
        (
            &api["project"],
            &api["logs_days"],
            &api["log_growth_per_day"]
        ),
        (&json!("api"), &json!(30), &json!("1.0 MB"))
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
