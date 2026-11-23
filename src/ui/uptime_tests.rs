//! The contract of the monitor routes.

use super::{Ui, ui};
use crate::{store, time};
use rusqlite::params;
use serde_json::{Value, json};

const HOUR: i64 = 3_600_000;
const DAY: i64 = 24 * HOUR;

/// A UI with project `shop`, an HTTP monitor and a heartbeat added 10 days
/// before `now`, and a person, whose cookie it returns.
fn monitored(now: i64) -> (Ui, String) {
    let s = ui();
    store::create_project(&s.db, "shop").unwrap();
    s.db.execute_batch(&format!(
        "INSERT INTO monitors(id,project_id,name,kind,url,expect,every_s,timeout_s,state,last_check_at,created_at)
         VALUES (1,1,'checkout','http','https://ann:pw@shop.example/health?token=s3cret#top','status',60,10,'down',{now},{created});
         INSERT INTO monitors(id,project_id,name,kind,heartbeat_token,every_s,grace_s,state,last_ping_at,created_at)
         VALUES (2,1,'backup','heartbeat','hb-secret-token',3600,300,'up',1700000000000,{created});",
        created = now - 10 * DAY,
    ))
    .unwrap();
    let cookie = s.person("ann@example.com", now);
    (s, cookie)
}

fn check(s: &Ui, at: i64, ok: bool, ms: Option<i64>) {
    s.db.execute(
        "INSERT INTO checks(monitor_id,at,ok,status,ms,reason) VALUES (1,?1,?2,?3,?4,?5)",
        params![
            at,
            ok,
            if ok { 200 } else { 503 },
            ms,
            (!ok).then_some("HTTP 503")
        ],
    )
    .unwrap();
}

fn incident(s: &Ui, started: i64, resolved: Option<i64>) {
    s.db.execute(
        "INSERT INTO incidents(monitor_id,started_at,resolved_at,reason) VALUES (1,?1,?2,'HTTP 503')",
        params![started, resolved],
    )
    .unwrap();
}

#[tokio::test]
async fn the_monitor_list_shows_each_hour_of_checks_and_hides_credentials() {
    let now = time::now_ms();
    let (s, cookie) = monitored(now);
    check(&s, now, true, Some(100));
    check(&s, now, true, Some(201));
    check(&s, now, false, None);
    check(&s, now - 2 * HOUR, true, Some(90));
    check(&s, now - 2 * DAY, true, Some(80));
    incident(&s, now - HOUR, None);
    let r = s.get("/ui/api/monitors", &[("cookie", &cookie)]).await;
    assert_eq!(r.status, 200);
    let monitors = r.body["monitors"].as_array().unwrap();
    let names: Vec<&str> = monitors
        .iter()
        .map(|m| m["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["backup", "checkout"]);
    let http = &monitors[1];
    assert_eq!(http["kind"], "http");
    assert_eq!(http["url"], "https://shop.example/health");
    assert_eq!(http["state"], "down");
    assert_eq!(http["every_s"], 60);
    assert_eq!(http["last_check"], time::rfc3339_ms(now));
    assert_eq!(http["open_since"], time::rfc3339_ms(now - HOUR));
    let hourly = http["hourly"].as_array().unwrap();
    assert_eq!(hourly.len(), 24);
    assert_eq!(hourly[23], json!({"checks": 3, "failed": 1, "ms": 151}));
    assert_eq!(hourly[21], json!({"checks": 1, "failed": 0, "ms": 90}));
    let checked: u64 = hourly.iter().map(|h| h["checks"].as_u64().unwrap()).sum();
    assert_eq!(checked, 4, "the check two days ago is outside the chart");
    let heartbeat = &monitors[0];
    assert_eq!(heartbeat["kind"], "heartbeat");
    assert_eq!(heartbeat["url"], Value::Null);
    let text = r.body.to_string();
    for secret in ["s3cret", "ann:pw", "hb-secret-token"] {
        assert!(!text.contains(secret), "{secret} is in {text}");
    }
    assert_eq!(
        s.get("/ui/api/monitors?project=shop", &[("cookie", &cookie)])
            .await
            .status,
        400
    );
}

#[tokio::test]
async fn one_monitor_has_its_uptime_incidents_and_newest_checks() {
    let now = time::now_ms();
    let (s, cookie) = monitored(now);
    for i in 0..60 {
        check(&s, now - i * 60_000, i % 2 == 0, Some(100 + i));
    }
    incident(&s, now - 2 * DAY, Some(now - 2 * DAY + 6 * HOUR));
    incident(&s, now - 20 * DAY, Some(now - 19 * DAY));
    let r = s.get("/ui/api/monitors/1", &[("cookie", &cookie)]).await;
    assert_eq!(r.status, 200);
    assert_eq!(r.body["monitor"]["name"], "checkout");
    let up7 = r.body["uptime_7d"].as_f64().unwrap();
    let up30 = r.body["uptime_30d"].as_f64().unwrap();
    assert!((up7 - (1.0 - 6.0 / 168.0)).abs() < 1e-6, "{up7}");
    assert!(
        (up30 - (1.0 - 6.0 / 240.0)).abs() < 1e-6,
        "the 30 days start when the monitor was added: {up30}"
    );
    let incidents = r.body["incidents"].as_array().unwrap();
    assert_eq!(incidents.len(), 2);
    assert_eq!(
        incidents[0],
        json!({
            "id": 1,
            "started": time::rfc3339_ms(now - 2 * DAY),
            "resolved": time::rfc3339_ms(now - 2 * DAY + 6 * HOUR),
            "reason": "HTTP 503",
        })
    );
    let checks = r.body["checks"].as_array().unwrap();
    assert_eq!(checks.len(), 50);
    assert_eq!(
        checks[0],
        json!({"at": time::rfc3339_ms(now), "ok": true, "status": 200, "ms": 100, "reason": null})
    );
    assert_eq!(
        checks[1],
        json!({"at": time::rfc3339_ms(now - 60_000), "ok": false, "status": 503, "ms": 101, "reason": "HTTP 503"})
    );
    assert_eq!(r.body["timeout_s"], 10);
    assert_eq!(r.body["expected_by"], Value::Null);

    let r = s.get("/ui/api/monitors/2", &[("cookie", &cookie)]).await;
    assert_eq!(r.body["last_ping"], "2023-11-14T22:13:20Z");
    assert_eq!(
        r.body["expected_by"], "2023-11-14T23:18:20Z",
        "a ping is due every hour, and late after 5 more minutes"
    );
    assert_eq!(r.body["uptime_7d"], 1.0);

    for (uri, status) in [
        ("/ui/api/monitors/3", 404),
        ("/ui/api/monitors/abc", 404),
        ("/ui/api/monitors/1?full=1", 400),
    ] {
        assert_eq!(
            s.get(uri, &[("cookie", &cookie)]).await.status,
            status,
            "{uri}"
        );
    }
}

#[test]
fn uptime_counts_only_the_part_of_each_incident_inside_the_window() {
    use super::super::monitors::uptime;
    assert_eq!(uptime(&[], 0, 1000), Some(1.0));
    assert_eq!(uptime(&[(100, Some(200))], 0, 1000), Some(0.9));
    assert_eq!(uptime(&[(-500, Some(250))], 0, 1000), Some(0.75));
    assert_eq!(uptime(&[(500, None)], 0, 1000), Some(0.5));
    assert_eq!(uptime(&[(100, Some(200)), (900, None)], 0, 1000), Some(0.8));
    assert_eq!(uptime(&[], 1000, 1000), None);
}
