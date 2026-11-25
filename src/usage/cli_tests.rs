//! `davy usage` in a terminal.

use super::*;
use crate::usage::tests::{data_with_counts, logs_pending};
use serde_json::json;

#[test]
fn davy_usage_prints_the_counts_of_the_window_as_json() {
    let data = data_with_counts("usage-cli");
    let mut out = Vec::new();
    let args = Args {
        data: data.clone(),
        project: Some("shop".into()),
        since: "7d".into(),
        logs_days: Some(3),
        logs_bytes_per_day: None,
        log_retention_days: 30,
        view: View::Summary,
        top: 3,
        json: true,
    };
    run(&mut out, &args).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    let b = &v["basis"];
    assert_eq!(v["no_forecast"], json!(null));
    assert_eq!(b["logs"]["basis"], "errors_only");
    assert_eq!(b["confidence"], "low");
    assert_eq!(b["why"], logs_pending());
    assert_eq!(
        v["what_if"],
        json!({"project":"shop","logs_days":3,"logs_bytes_per_day":null})
    );
    assert_eq!(
        v["what_if_note"],
        "this forecast counts logs as 0, as it rests on errors alone, so a log retention changes it only by the log days on disk it deletes"
    );
    assert_eq!(v["projects"].as_array().unwrap().len(), 1);
    let shop = &v["projects"][0];
    assert_eq!(
        (&shop["project"], &shop["logs_days"], &shop["errors_days"]),
        (&json!("shop"), &json!(7), &json!(36_500))
    );
    assert_eq!(shop["signals"]["logs"]["records"], 1_000);
    assert_eq!(shop["signals"]["logs"]["bytes"], 2_500_000);
    assert_eq!(shop["signals"]["errors"]["records"], 0);
    assert_eq!(
        shop["logs_stored"]["records"], 1_000,
        "tomorrow's index too"
    );
    assert_eq!(shop["logs_stored"]["bytes"], 2_001_000);
    assert!(v["disk"]["total"].as_u64().unwrap() > 0);
    std::fs::remove_dir_all(&data).unwrap();
}

#[test]
fn davy_usage_prints_text_by_default_and_refuses_a_long_window() {
    let data = data_with_counts("usage-text");
    let mut args = Args {
        data: data.clone(),
        project: None,
        since: "7d".into(),
        logs_days: None,
        logs_bytes_per_day: None,
        log_retention_days: 30,
        view: View::Summary,
        top: 3,
        json: false,
    };
    let mut out = Vec::new();
    run(&mut out, &args).unwrap();
    let text = String::from_utf8(out).unwrap();
    let why = logs_pending();
    let line = format!("basis     errors_only, low confidence: {why}\n");
    assert!(text.contains(&line), "{text}");
    let api_errors = format!("{:<20} {:<11} {:>10} {:>10}", "api", "errors", 3, "3.0 KB");
    assert!(text.contains(&api_errors), "{text}");
    let shop_stored = format!(
        "{:<20} {:<11} {:>10} {:>10}",
        "shop", "logs_stored", 1_000, "2.0 MB"
    );
    assert!(text.contains(&shop_stored), "{text}");
    assert!(
        text.contains("shop                 keeps logs 7 days, errors 36500 days\n"),
        "{text}"
    );
    args.since = "91d".into();
    let err = run(&mut Vec::new(), &args).unwrap_err();
    assert_eq!(err.to_string(), "since reaches back at most 90d");
    std::fs::remove_dir_all(&data).unwrap();
}

/// `davy usage` as the binary parses it.
#[derive(clap::Parser)]
struct Cli {
    #[command(flatten)]
    usage: Args,
}

#[test]
fn davy_usage_view_savings_prints_the_entries_and_bounds_top() {
    let data = data_with_counts("usage-savings");
    let mut args = Args {
        data: data.clone(),
        project: None,
        since: "7d".into(),
        logs_days: None,
        logs_bytes_per_day: None,
        log_retention_days: 30,
        view: View::Savings,
        top: 3,
        json: false,
    };
    let mut out = Vec::new();
    run(&mut out, &args).unwrap();
    let text = String::from_utf8(out).unwrap();
    let since = time::day_of(time::now_ns())
        .checked_sub(jiff::ToSpan::days(7))
        .unwrap();
    assert!(
        text.starts_with(&format!("window    since {since} (")),
        "{text}"
    );
    assert!(
        text.contains("savings   nothing to suggest\nnote      last_queried is approximate"),
        "{text}"
    );
    args.json = true;
    let mut out = Vec::new();
    run(&mut out, &args).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["savings"], json!([]));
    assert_eq!(v["note"], savings::NOTE);
    let parse = |top: &str| {
        <Cli as clap::Parser>::try_parse_from([
            "davy", "--data", "d", "--view", "savings", "--top", top,
        ])
        .map(|cli| cli.usage.top)
        .map_err(|e| e.kind())
    };
    assert_eq!(parse("20"), Ok(20));
    assert_eq!(parse("21"), Err(clap::error::ErrorKind::ValueValidation));
    assert_eq!(parse("0"), Err(clap::error::ErrorKind::ValueValidation));
    std::fs::remove_dir_all(&data).unwrap();
}

#[test]
fn lines_live_drop_rules_dropped_show_as_logs_filtered() {
    let data = data_with_counts("usage-filtered");
    let c = crate::store::open(&data).unwrap();
    let today = time::day_of(time::now_ns());
    let key = |rule, live| crate::drops::Key {
        day: today,
        rule,
        live,
        project: 2,
    };
    crate::drops::write(
        &c,
        &[
            (key(1, true), crate::usage::Count::new(400, 40_000)),
            (key(2, false), crate::usage::Count::new(90, 9_000)),
        ],
    )
    .unwrap();
    let mut args = Args {
        data: data.clone(),
        project: Some("shop".into()),
        since: "7d".into(),
        logs_days: None,
        logs_bytes_per_day: None,
        log_retention_days: 30,
        view: View::Summary,
        top: 3,
        json: true,
    };
    let mut out = Vec::new();
    run(&mut out, &args).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    let filtered = &v["projects"][0]["logs_filtered"];
    // A shadow rule's lines were stored, so they are not filtered.
    assert_eq!(
        (&filtered["records"], &filtered["bytes"]),
        (&json!(400), &json!(40_000))
    );
    args.json = false;
    let mut out = Vec::new();
    run(&mut out, &args).unwrap();
    let text = String::from_utf8(out).unwrap();
    let row = format!(
        "{:<20} {:<11} {:>10} {:>10}",
        "shop", "logs_filtered", 400, "40.0 KB"
    );
    assert!(text.contains(&row), "{text}");
    std::fs::remove_dir_all(&data).unwrap();
}
