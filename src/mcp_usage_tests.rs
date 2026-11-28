//! The `usage` tool.

use super::*;

/// The keys of the usage tool's `disk`.
const KEYS: [&str; 13] = [
    "basis",
    "busy_refused_requests_today_no_project",
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

const DISK_KEYS: [&str; 13] = [
    "beyond_10_years",
    "days_left",
    "free",
    "full_on",
    "headroom",
    "headroom_reason",
    "partial",
    "past_limit",
    "room_left",
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
        drops: Vec::new(),
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
            "limits",
            "logs_days",
            "logs_filtered",
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
async fn usage_shows_each_projects_quota_today_and_its_refusals() {
    crate::test_support::away_from_midnight().await;
    let s = server();
    let c = store::open(&s.data).unwrap();
    let quota = crate::quota::Settings {
        logs: Some(2_000_000_000),
        ..crate::quota::Settings::default()
    };
    crate::quota::settings::store(&c, 1, &quota).unwrap();
    let now = time::now_ns();
    let row = usage::Row {
        day: time::day_of(now),
        project: 1,
        signal: usage::Signal::Logs,
        count: usage::Count {
            records: 10,
            bytes: 1_200_000_000,
        },
    };
    usage::write(&c, &[row]).unwrap();
    let midnight_ms = time::midnight(time::day_of(now)) / 1_000_000;
    let own = crate::items::client_report::add_own;
    own(&c, 1, midnight_ms, crate::quota::QUOTA, "log_item", 2).unwrap();
    let (text, error) = tool(&s.app, "usage", json!({"since": "1d"})).await;
    assert!(!error, "{text}");
    let v: Value = serde_json::from_str(&text).unwrap();
    let resets_at = time::rfc3339(time::next_midnight(now));
    assert_eq!(
        v["projects"][0]["limits"],
        json!({
            "errors": null,
            "logs": {"quota": "2.0 GB", "today": "1.2 GB", "percent": 60, "refusing": false},
            "warn_percent": 80,
            "spike": {
                "errors": {"setting": "default", "per_second": 4000, "refusing": false},
                "logs": {"setting": "off", "per_second": null, "refusing": false},
            },
            "resets_at": resets_at,
            "refused_today": {"davy_quota": 2, "davy_spike": 0},
            "refused_log_requests_today": {"davy_quota": 0, "davy_spike": 0},
            "busy_refused_requests_today": 0,
        })
    );
}

/// The live gate knows a spike bucket and today's counts not flushed yet,
/// which the store does not.
#[tokio::test]
async fn usage_says_what_the_running_server_refuses_now() {
    crate::test_support::away_from_midnight().await;
    let gate = Arc::new(crate::quota::Gate::default());
    let s = server_with(usage::Shared::with_gate(Arc::clone(&gate)));
    let c = store::open(&s.data).unwrap();
    let quota = crate::quota::Settings {
        logs: Some(1_000),
        spike: crate::quota::Spike::Rate(1),
        ..crate::quota::Settings::default()
    };
    crate::quota::settings::store(&c, 1, &quota).unwrap();
    let now = time::now_ns();
    gate.load(std::collections::HashMap::from([(1, quota)]), now);
    gate.used(1, usage::Signal::Logs, usage::Count::new(1, 1_000), now);
    gate.used(1, usage::Signal::Errors, usage::Count::new(11, 1), now);
    let (text, error) = tool(&s.app, "usage", json!({"since": "1d"})).await;
    assert!(!error, "{text}");
    let v: Value = serde_json::from_str(&text).unwrap();
    let limits = &v["projects"][0]["limits"];
    let refusing = |path: &[&str]| path.iter().fold(limits, |v, k| &v[*k])["refusing"].clone();
    assert_eq!(
        [
            refusing(&["logs"]),
            refusing(&["spike", "errors"]),
            refusing(&["spike", "logs"]),
        ],
        [json!(true), json!(true), json!(false)]
    );
    assert_eq!(limits["logs"]["today"], "0 B", "the store has none yet");
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

#[tokio::test]
async fn search_logs_and_count_logs_record_the_search_for_the_savings_view() {
    use crate::usage::savings::searches::By;
    let s = server();
    let filter = json!({"query": "card declined", "project": "api", "level": "warn"});
    let (text, is_error) = tool(&s.app, "search_logs", filter.clone()).await;
    assert!(!is_error, "{text}");
    let (text, is_error) = tool(&s.app, "count_logs", json!({"pattern": "other"})).await;
    assert!(!is_error, "{text}");
    s.settled().await;
    let searches: Vec<_> = s
        .writes
        .try_iter()
        .filter_map(|w| match w.job {
            Job::Search { search, .. } => Some(search),
            _ => None,
        })
        .collect();
    let shape: Vec<_> = searches
        .iter()
        .map(|s| (s.by, s.project, s.level, s.pattern, s.query.as_deref()))
        .collect();
    assert_eq!(
        shape,
        [
            (By::Agent, Some(1), Some(4), None, Some("card declined")),
            (By::Agent, None, None, Some(patterns::OTHER), None),
        ]
    );
    assert_eq!(searches[0].until - searches[0].since, 86_400_000_000_000);
}

#[tokio::test]
async fn usage_view_savings_shows_a_noisy_issue_with_the_command_a_person_runs() {
    let s = server();
    let c = store::open(&s.data).unwrap();
    let now = time::now_ms();
    let mut e = crate::event::ErrorEvent::parse(json!({
        "event_id": "a0000000000000000000000000000001",
        "message": "payment declined",
    }))
    .unwrap();
    (e.received_at, e.timestamp) = (now, now);
    store::insert(&c, 1, &e).unwrap();
    c.execute("UPDATE issue_hourly SET count=150", []).unwrap();
    // top 0 shows one entry of each kind.
    let ask = json!({"view": "savings", "top": 0});
    let (text, is_error) = tool(&s.app, "usage", ask).await;
    assert!(!is_error, "{text}");
    let v: Value = serde_json::from_str(&text).unwrap();
    let data = crate::install::sh(&s.data.display().to_string());
    assert_eq!(
        v["savings"],
        json!([{
            "kind": "issue", "id": 1, "project": "api", "title": "payment declined",
            "events": 150, "share": 1.0,
            "suggest": format!("fix it, or a person can discard it, which also stops storing its real events: davy issue discard 1 --data {data}"),
            "command": format!("davy issue discard 1 --data {data}"),
            "try": "issue_context 1",
        }])
    );
    assert_eq!(v["note"], usage::savings::NOTE);
    let keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
    assert_eq!(keys, ["note", "savings", "window"]);
    let (text, is_error) = tool(&s.app, "usage", json!({"view": "bills"})).await;
    assert!(is_error, "{text}");
}

#[tokio::test]
async fn usage_past_the_reads_at_once_is_refused_as_busy() {
    let shared = usage::Shared::default();
    let s = server_with(shared.clone());
    let held: Vec<_> = (0..usage::AT_ONCE)
        .map(|_| shared.permit().unwrap())
        .collect();
    let (text, error) = tool(&s.app, "usage", json!({})).await;
    assert_eq!((text.as_str(), error), (usage::BUSY, true));
    drop(held);
    let (text, error) = tool(&s.app, "usage", json!({})).await;
    assert!(!error, "{text}");
}
