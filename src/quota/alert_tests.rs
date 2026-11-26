//! Quota alerts: the gate notices each line a project crosses once a day,
//! and the writer sends each once, across flushes and restarts.

use super::*;
use crate::{
    alert::{Destination, Kind, Notice, Quota},
    monitor::CheckUrl,
    store,
    usage::Count,
};
use jiff::civil::date;
use std::collections::HashMap;

/// 2026-11-26 00:00 UTC, and the midnight after it.
const MIDNIGHT: i64 = 1_795_651_200_000_000_000;
const NEXT: i64 = 1_795_737_600_000_000_000;
const HOUR: i64 = 3_600_000_000_000;
const NOON: i64 = MIDNIGHT + 12 * HOUR;

fn memory() -> Connection {
    let mut c = Connection::open_in_memory().unwrap();
    store::init(&mut c).unwrap();
    store::create_project(&c, "shop").unwrap();
    c
}

fn errors(quota: u64) -> Settings {
    Settings {
        errors: Some(quota),
        ..Settings::default()
    }
}

fn gate(s: Settings) -> Gate {
    let g = Gate::default();
    g.load(HashMap::from([(1, s)]), NOON);
    g
}

/// The alerts of the next flush as `(signal, kind, used)`.
fn alerts(g: &Gate) -> Vec<(Signal, &'static str, u64)> {
    g.take()
        .alerts
        .into_iter()
        .map(|a| (a.signal, a.crossed.name(), a.used))
        .collect()
}

fn alert(crossed: Crossed, project: i64) -> Alert {
    Alert {
        day: date(2026, 11, 26),
        project,
        signal: Signal::Logs,
        crossed,
        used: 2_000_000_000,
        quota: 2_000_000_000,
        warn: 80,
        at: NOON,
        resets_at: NEXT,
    }
}

#[test]
fn a_quota_warns_once_at_its_percent_and_alerts_once_when_reached() {
    let g = gate(errors(10));
    g.used(1, Signal::Errors, Count::new(7, 1), NOON);
    assert_eq!(alerts(&g), []);
    g.used(1, Signal::Errors, Count::new(1, 1), NOON);
    g.used(1, Signal::Errors, Count::new(1, 1), NOON);
    assert_eq!(alerts(&g), [(Signal::Errors, "warn", 8)]);
    g.used(1, Signal::Errors, Count::new(1, 1), NOON);
    g.used(1, Signal::Errors, Count::new(5, 1), NOON);
    assert_eq!(alerts(&g), [(Signal::Errors, "reached", 10)]);
    g.used(1, Signal::Errors, Count::new(5, 1), NOON);
    assert_eq!(alerts(&g), []);
}

#[test]
fn a_jump_past_both_lines_or_a_flush_with_both_alerts_reached_alone() {
    let logs = Settings {
        logs: Some(100),
        ..Settings::default()
    };
    let g = gate(logs);
    g.used(1, Signal::Logs, Count::new(1, 50), NOON);
    g.used(1, Signal::Logs, Count::new(1, 60), NOON);
    assert_eq!(alerts(&g), [(Signal::Logs, "reached", 110)]);
    let g = gate(errors(10));
    g.used(1, Signal::Errors, Count::new(8, 1), NOON);
    g.used(1, Signal::Errors, Count::new(2, 1), NOON);
    assert_eq!(alerts(&g), [(Signal::Errors, "reached", 10)]);
}

#[test]
fn a_lower_quota_alerts_when_the_settings_are_read() {
    let g = Gate::default();
    g.used(1, Signal::Errors, Count::new(500, 1), NOON);
    assert_eq!(alerts(&g), []);
    g.load(HashMap::from([(1, errors(100))]), NOON);
    assert_eq!(alerts(&g), [(Signal::Errors, "reached", 500)]);
    g.load(HashMap::from([(1, errors(100))]), NOON);
    assert_eq!(alerts(&g), []);
}

#[test]
fn a_new_utc_day_alerts_again_and_a_failed_flush_is_kept() {
    let g = gate(errors(1));
    g.used(1, Signal::Errors, Count::new(1, 1), NOON);
    let flush = g.take();
    assert_eq!(flush.alerts[0].resets_at, NEXT);
    g.put_back(flush);
    assert_eq!(alerts(&g), [(Signal::Errors, "reached", 1)]);
    g.used(1, Signal::Errors, Count::new(1, 1), NEXT);
    let flush = g.take();
    assert_eq!(flush.alerts[0].day, date(2026, 11, 27));
    assert_eq!(flush.alerts[0].used, 1);
}

#[test]
fn a_restart_does_not_hand_the_writer_an_alert_sent_today() {
    let c = memory();
    settings::store(&c, 1, &errors(10)).unwrap();
    let row = |day| crate::usage::Row {
        day,
        project: 1,
        signal: Signal::Errors,
        count: Count::new(10, 1),
    };
    crate::usage::write(&c, &[row(date(2026, 11, 26))]).unwrap();
    let g = Gate::default();
    start(&c, &g, NOON).unwrap();
    let flush = g.take();
    assert_eq!(flush.alerts.len(), 1);
    write(&c, &flush).unwrap();
    let g = Gate::default();
    start(&c, &g, NOON).unwrap();
    assert_eq!(alerts(&g), []);
}

#[test]
fn a_raised_quota_alerts_again_when_reached_also_after_a_restart() {
    let c = memory();
    let g = gate(errors(10));
    g.used(1, Signal::Errors, Count::new(10, 1), NOON);
    let flush = g.take();
    assert_eq!(flush.alerts.len(), 1);
    write(&c, &flush).unwrap();
    g.load(HashMap::from([(1, errors(20))]), NOON);
    assert_eq!(alerts(&g), []);
    g.used(1, Signal::Errors, Count::new(6, 1), NOON);
    let flush = g.take();
    write(&c, &flush).unwrap();
    let crossed: Vec<_> = flush
        .alerts
        .iter()
        .map(|a| (a.crossed.name(), a.used, a.quota))
        .collect();
    assert_eq!(crossed, [("warn", 16, 20)]);
    g.used(1, Signal::Errors, Count::new(4, 1), NOON);
    let flush = g.take();
    write(&c, &flush).unwrap();
    let crossed: Vec<_> = flush
        .alerts
        .iter()
        .map(|a| (a.crossed.name(), a.used, a.quota))
        .collect();
    assert_eq!(crossed, [("reached", 20, 20)]);
    let marks: Vec<(String, i64)> = c
        .prepare("SELECT kind, quota FROM quota_alerts ORDER BY quota, kind")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let mark = |kind: &str, quota| (kind.to_owned(), quota);
    assert_eq!(
        marks,
        [
            mark("reached", 10),
            mark("warn", 10),
            mark("reached", 20),
            mark("warn", 20)
        ]
    );
    crate::usage::write(
        &c,
        &[crate::usage::Row {
            day: date(2026, 11, 26),
            project: 1,
            signal: Signal::Errors,
            count: Count::new(20, 1),
        }],
    )
    .unwrap();
    settings::store(&c, 1, &errors(20)).unwrap();
    let g = Gate::default();
    start(&c, &g, NOON).unwrap();
    assert_eq!(alerts(&g), []);
    settings::store(&c, 1, &errors(15)).unwrap();
    let g = Gate::default();
    start(&c, &g, NOON).unwrap();
    assert_eq!(alerts(&g), [(Signal::Errors, "reached", 20)]);
}

#[test]
fn the_writer_sends_an_alert_once_and_marks_the_warning_with_the_reach() {
    let c = memory();
    let to = Destination::Webhook(CheckUrl::parse("https://hooks.example/x").unwrap());
    alert::store::add(&c, Some(1), &to, false, 1_000).unwrap();
    let flush = Flush {
        alerts: vec![alert(Crossed::Reached, 1), alert(Crossed::Reached, 99)],
        ..Flush::default()
    };
    write(&c, &flush).unwrap();
    write(&c, &flush).unwrap();
    let late_warning = Flush {
        alerts: vec![alert(Crossed::Warn, 1)],
        ..Flush::default()
    };
    write(&c, &late_warning).unwrap();
    let sent: Vec<(String, i64)> = c
        .prepare("SELECT type, created_at FROM notifications")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(sent, [("quota.reached".to_owned(), 1_795_694_400_000)]);
    let marks: Vec<(String, i64, String, String, i64)> = c
        .prepare("SELECT day, project_id, signal, kind, quota FROM quota_alerts ORDER BY kind")
        .unwrap()
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let mark = |kind: &str| {
        (
            "2026-11-26".to_owned(),
            1,
            "logs".to_owned(),
            kind.to_owned(),
            2_000_000_000,
        )
    };
    assert_eq!(marks, [mark("reached"), mark("warn")]);
}

#[test]
fn marks_older_than_a_week_are_pruned() {
    let c = memory();
    for day in ["2026-11-18", "2026-11-19", "2026-11-26"] {
        c.execute(
            "INSERT INTO quota_alerts VALUES (?1, 1, 'logs', 'warn', 100)",
            [day],
        )
        .unwrap();
    }
    prune(&c, NOON / 1_000_000).unwrap();
    let left: Vec<String> = c
        .prepare("SELECT day FROM quota_alerts ORDER BY day")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(left, ["2026-11-19", "2026-11-26"]);
}

fn logs_quota(used: u64, percent: u64) -> Quota {
    Quota {
        signal: "logs",
        used,
        quota: 2_000_000_000,
        percent,
        warn_percent: 80,
        resets_at: "2026-11-27T00:00:00Z".to_owned(),
    }
}

#[test]
fn quota_notices_render_for_each_channel_kind() {
    let warn = Notice::QuotaWarn(logs_quota(1_600_000_000, 80));
    let body: serde_json::Value = serde_json::from_str(
        &warn
            .render(Kind::Webhook, 31, Some("shop"), 1_795_701_731_000)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        body,
        serde_json::json!({
            "id": 31, "type": "quota.warn", "project": "shop", "at": "2026-11-26T14:02:11Z",
            "quota": {"signal": "logs", "used": 1_600_000_000_u64, "quota": 2_000_000_000_u64,
                "percent": 80, "warn_percent": 80, "resets_at": "2026-11-27T00:00:00Z"},
        })
    );
    let slack = |n: &Notice| {
        let v: serde_json::Value =
            serde_json::from_str(&n.render(Kind::Slack, 1, Some("shop"), 0).unwrap()).unwrap();
        v["text"].as_str().unwrap().to_owned()
    };
    assert_eq!(
        slack(&warn),
        "[shop] Logs at 80% of the daily quota: 1.6 GB of 2.0 GB"
    );
    let reached = Notice::QuotaReached(logs_quota(2_000_000_000, 100));
    assert_eq!(
        slack(&reached),
        "[shop] Logs reached the daily quota of 2.0 GB, refused until midnight UTC"
    );
    let errors = Notice::QuotaWarn(Quota {
        signal: "errors",
        used: 4000,
        quota: 5000,
        ..logs_quota(0, 80)
    });
    assert_eq!(
        slack(&errors),
        "[shop] Errors at 80% of the daily quota: 4000 of 5000 events"
    );
    let mail: alert::Mail =
        serde_json::from_str(&reached.render(Kind::Email, 7, Some("shop"), 0).unwrap()).unwrap();
    assert_eq!(
        mail.text,
        "[shop] Logs reached the daily quota of 2.0 GB, refused until midnight UTC\n\nsignal: logs\nused: 2000000000\nquota: 2000000000\npercent: 100\nwarn_percent: 80\nresets_at: 2026-11-27T00:00:00Z\nat: 1970-01-01T00:00:00Z\n"
    );
}
