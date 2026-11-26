//! The contract of the usage routes.

use super::{Ui, ui};
use crate::{store, time};
use serde_json::json;

/// Waits out the last seconds of a UTC day for the next one to begin, so
/// a test that reads today before its request and the server that reads
/// it again agree. It yields until the day turns, as only that ends it.
async fn away_from_midnight() {
    let start = time::now_ns();
    if time::DAY_NS - start.rem_euclid(time::DAY_NS) < 10_000_000_000 {
        let day = time::day_of(start);
        while time::day_of(time::now_ns()) == day {
            tokio::task::yield_now().await;
        }
    }
}

/// Masks what the web UI and the CLI measure at their own moment: the
/// disk, and Davy's own share of it.
fn timeless(mut v: serde_json::Value) -> serde_json::Value {
    for key in ["disk", "disk_error", "davy"] {
        v.as_object_mut().unwrap().remove(key);
    }
    v
}

fn counted(s: &Ui, day: &str, signal: &str, records: i64, bytes: i64) {
    s.db.execute(
        "INSERT INTO usage_daily(day, project_id, signal, records, bytes) VALUES (?1, 1, ?2, ?3, ?4)",
        (day, signal, records, bytes),
    )
    .unwrap();
}

#[tokio::test]
async fn the_summary_is_what_davy_usage_json_prints() {
    away_from_midnight().await;
    let s = ui();
    store::create_project(&s.db, "shop").unwrap();
    let today = time::day_of(time::now_ns()).to_string();
    counted(&s, &today, "errors", 4, 2_000);
    let cookie = s.person("ann@example.com", time::now_ms());
    let r = s
        .get("/ui/api/usage?since=1d", &[("cookie", &cookie)])
        .await;
    assert_eq!(r.status, 200);
    let shop = &r.body["projects"][0];
    assert_eq!(shop["project"], "shop");
    assert_eq!(shop["signals"]["errors"]["records"], 4);
    assert_eq!(shop["signals"]["errors"]["bytes"], 2_000);
    assert_eq!(shop["errors_discarded"], 0);
    // Sizes are bytes, as with --json, never text such as "2.0 kB".
    if r.body["disk_error"].is_null() {
        assert!(r.body["disk"]["total"].is_u64(), "{}", r.body);
        assert!(r.body["disk"]["room_left"].is_u64(), "{}", r.body);
    } else {
        assert!(r.body["disk_error"].is_string(), "{}", r.body);
    }
    let mut cli = Vec::new();
    crate::usage::run(
        &mut cli,
        &crate::usage::Args {
            data: s.reads.data().to_owned(),
            project: None,
            since: "1d".to_owned(),
            logs_days: None,
            logs_bytes_per_day: None,
            log_retention_days: crate::retention::DEFAULT_LOG_DAYS,
            view: crate::usage::savings::View::Summary,
            top: crate::usage::savings::DEFAULT_TOP,
            json: true,
        },
    )
    .unwrap();
    let cli: serde_json::Value = serde_json::from_slice(&cli).unwrap();
    let fields = crate::test_support::assert_usage_ts_fields;
    fields("UsageReport", &r.body);
    fields("UsageProject", shop);
    fields("Count", &shop["signals"]["errors"]);
    if r.body["disk_error"].is_null() {
        fields("Disk", &r.body["disk"]);
    }
    assert_eq!(timeless(r.body), timeless(cli));
}

#[tokio::test]
async fn past_the_reads_at_once_both_routes_answer_429() {
    let s = ui();
    let cookie = s.person("ann@example.com", time::now_ms());
    let held: Vec<_> = (0..crate::usage::AT_ONCE)
        .map(|_| s.reads.usage().permit().unwrap())
        .collect();
    for uri in ["/ui/api/usage", "/ui/api/usage/days"] {
        let r = s.get(uri, &[("cookie", &cookie)]).await;
        assert_eq!(
            (r.status, r.header("retry-after"), &r.body),
            (429, "1", &json!({ "error": crate::usage::BUSY })),
            "{uri}"
        );
    }
    drop(held);
    let r = s.get("/ui/api/usage/days", &[("cookie", &cookie)]).await;
    assert_eq!(r.status, 200);
}

#[tokio::test]
async fn the_savings_view_lists_entries_and_its_note() {
    away_from_midnight().await;
    let s = ui();
    store::create_project(&s.db, "shop").unwrap();
    let cookie = s.person("ann@example.com", time::now_ms());
    let r = s
        .get(
            "/ui/api/usage?view=savings&project=shop&top=5",
            &[("cookie", &cookie)],
        )
        .await;
    assert_eq!(r.status, 200);
    assert_eq!(r.body["savings"], json!([]));
    let since = time::day_of(time::now_ns() - 7 * time::DAY_NS).to_string();
    assert_eq!(r.body["window"]["since"], since);
    assert_eq!(r.body["note"], crate::usage::savings::NOTE);
}

#[tokio::test]
async fn bad_parameters_are_refused_with_the_reason() {
    let s = ui();
    store::create_project(&s.db, "shop").unwrap();
    let cookie = s.person("ann@example.com", time::now_ms());
    for (uri, status, error) in [
        ("/ui/api/usage?view=pie", 400, "view is summary or savings"),
        ("/ui/api/usage?top=0", 400, "top is 1 to 20"),
        ("/ui/api/usage?top=21", 400, "top is 1 to 20"),
        (
            "/ui/api/usage?since=91d",
            400,
            "since reaches back at most 90d",
        ),
        (
            "/ui/api/usage?what_if=1",
            400,
            "this route takes only view, project, since, top",
        ),
        (
            "/ui/api/usage?project=nope",
            404,
            "unknown project \"nope\"",
        ),
        (
            "/ui/api/usage/days?project=nope",
            404,
            "unknown project \"nope\"",
        ),
        (
            "/ui/api/usage/days?since=91d",
            400,
            "since reaches back at most 90d",
        ),
        (
            "/ui/api/usage/days?top=3",
            400,
            "this route takes only project, since",
        ),
    ] {
        let r = s.get(uri, &[("cookie", &cookie)]).await;
        assert_eq!(
            (r.status, &r.body),
            (status, &json!({ "error": error })),
            "{uri}"
        );
    }
}

#[tokio::test]
async fn days_lists_each_project_per_day_of_the_window() {
    away_from_midnight().await;
    let s = ui();
    store::create_project(&s.db, "shop").unwrap();
    let now = time::now_ns();
    let today = time::day_of(now);
    let yesterday = today.yesterday().unwrap();
    counted(&s, &today.to_string(), "logs", 5, 500);
    counted(&s, &yesterday.to_string(), "sessions", 2, 80);
    let cookie = s.person("ann@example.com", time::now_ms());
    let r = s
        .get("/ui/api/usage/days?since=1d", &[("cookie", &cookie)])
        .await;
    assert_eq!(r.status, 200);
    assert_eq!(
        r.body,
        json!({
            "days": [yesterday.to_string(), today.to_string()],
            "names": ["shop"],
            "other": null,
            "projects": [{
                "project": "shop",
                "stored": [80, 0],
                "accepted": [2, 5],
                "filtered": [0, 0],
                "discarded": [0, 0],
                "limited": [0, 0],
                "refused": [0, 0],
                "unreadable": [0, 0],
                "sdk_dropped": [0, 0],
            }],
        })
    );
    let r = s.get("/ui/api/usage/days", &[("cookie", &cookie)]).await;
    assert_eq!(r.body["days"].as_array().unwrap().len(), 31);
}

#[test]
fn the_usage_ts_fields_are_read_from_the_type() {
    assert_eq!(
        crate::test_support::usage_ts_fields("Count"),
        ["records", "bytes", "records_per_day", "bytes_per_day"]
    );
    let report = crate::test_support::usage_ts_fields("UsageReport");
    assert_eq!(&report[..3], ["window", "growth_per_day", "forecast"]);
    assert!(report.contains(&"disk?".to_owned()), "{report:?}");
}
