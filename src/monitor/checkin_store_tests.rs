use super::{
    NewMonitor, State, Target,
    checkin::{self, MonitorSlug},
    checkin_store::{MAX_SDK_MONITORS, apply},
    store,
};
use rusqlite::{Connection, params};
use std::time::Duration;

pub(super) fn memory() -> Connection {
    let mut c = Connection::open_in_memory().unwrap();
    crate::store::init(&mut c).unwrap();
    crate::store::create_project(&c, "shop").unwrap();
    c
}

pub(super) fn ms(s: &str) -> i64 {
    s.parse::<jiff::Timestamp>().unwrap().as_millisecond()
}

fn at(ms: Option<i64>) -> Option<String> {
    ms.map(|ms| jiff::Timestamp::from_millisecond(ms).unwrap().to_string())
}

const HOURLY: &str = r#"{"schedule":{"type":"crontab","value":"0 * * * *"},"checkin_margin":2,"max_runtime":10,"timezone":"Europe/Prague"}"#;
const RUN: &str = "9bcd4f844e1344919408cda90724bc82";

/// Applies a check-in for `slug` to project 1 at `when`, as the SDK would
/// send it with `extra` fields.
pub(super) fn send(c: &Connection, when: &str, slug: &str, status: &str, extra: &str) {
    let json = format!(r#"{{"monitor_slug":"{slug}","status":"{status}"{extra}}}"#);
    apply(c, 1, ms(when), &checkin::parse(json.as_bytes())).unwrap();
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Row {
    state: State,
    source: String,
    every_s: i64,
    cron: Option<String>,
    grace_s: Option<i64>,
    max_runtime_s: Option<i64>,
    run_id: Option<String>,
    pub(super) due_at: Option<String>,
}

pub(super) fn row(c: &Connection, slug: &str) -> Row {
    c.query_row(
        "SELECT state,source,every_s,cron,grace_s,max_runtime_s,run_id,due_at FROM monitors WHERE slug=?1",
        [slug],
        |r| {
            Ok(Row {
                state: r.get(0)?,
                source: r.get(1)?,
                every_s: r.get(2)?,
                cron: r.get(3)?,
                grace_s: r.get(4)?,
                max_runtime_s: r.get(5)?,
                run_id: r.get(6)?,
                due_at: at(r.get(7)?),
            })
        },
    )
    .unwrap()
}

fn sdk(state: State, run_id: Option<&str>, due_at: Option<&str>) -> Row {
    Row {
        state,
        source: "sdk".into(),
        every_s: 0,
        cron: Some("0 * * * *".into()),
        grace_s: Some(120),
        max_runtime_s: Some(600),
        run_id: run_id.map(Into::into),
        due_at: due_at.map(Into::into),
    }
}

/// Every check row of `slug`: whether it was up, its ms and its reason.
fn checks(c: &Connection, slug: &str) -> Vec<(bool, Option<i64>, Option<String>)> {
    c.prepare("SELECT ok,ms,reason FROM checks JOIN monitors m ON m.id=monitor_id WHERE m.slug=?1 ORDER BY at")
        .unwrap()
        .query_map([slug], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn incidents(c: &Connection) -> Vec<(String, Option<String>, String)> {
    c.prepare("SELECT started_at,resolved_at,reason FROM incidents ORDER BY id")
        .unwrap()
        .query_map([], |r| {
            Ok((at(r.get(0)?).unwrap(), at(r.get(1)?), r.get(2)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn dropped(c: &Connection) -> Vec<(String, i64)> {
    c.prepare("SELECT reason,quantity FROM client_reports WHERE project_id=1 AND category='monitor' ORDER BY reason")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

#[test]
fn a_run_creates_the_monitor_and_its_end_sets_the_next_expected_time() {
    let c = memory();
    let config = format!(r#","check_in_id":"{RUN}","monitor_config":{HOURLY}"#);
    send(
        &c,
        "2026-11-23T16:00:05Z",
        "nightly-report",
        "in_progress",
        &config,
    );
    let running = sdk(State::Pending, Some(RUN), Some("2026-11-23T16:10:05Z"));
    assert_eq!(row(&c, "nightly-report"), running);
    assert_eq!(checks(&c, "nightly-report"), []);
    let end = format!(r#","check_in_id":"{RUN}","duration":30.25"#);
    send(&c, "2026-11-23T16:00:35Z", "nightly-report", "ok", &end);
    assert_eq!(
        row(&c, "nightly-report"),
        sdk(State::Up, None, Some("2026-11-23T17:02:00Z"))
    );
    assert_eq!(checks(&c, "nightly-report"), [(true, Some(30_250), None)]);
    let listed = store::list(&c).unwrap();
    let [m] = &listed[..] else {
        panic!("{listed:?}")
    };
    assert_eq!(
        (
            m.name.as_str(),
            m.target.as_str(),
            m.slug.as_deref(),
            m.source.as_str(),
            m.schedule.as_deref()
        ),
        (
            "nightly-report",
            "check-in nightly-report",
            Some("nightly-report"),
            "sdk",
            Some("cron 0 * * * *, Europe/Prague")
        )
    );
    assert_eq!(dropped(&c), []);
}

#[test]
fn a_missed_check_in_opens_an_incident_and_the_next_one_closes_it() {
    let c = memory();
    send(
        &c,
        "2026-11-23T16:00:35Z",
        "nightly-report",
        "ok",
        &format!(r#","monitor_config":{HOURLY}"#),
    );
    store::sweep(&c, ms("2026-11-23T17:02:00Z")).unwrap();
    assert_eq!(row(&c, "nightly-report").state, State::Up);
    store::sweep(&c, ms("2026-11-23T17:02:01Z")).unwrap();
    assert_eq!(row(&c, "nightly-report"), sdk(State::Down, None, None));
    // Down already, so a later sweep opens nothing more.
    store::sweep(&c, ms("2026-11-23T19:00:00Z")).unwrap();
    send(&c, "2026-11-23T19:00:20Z", "nightly-report", "ok", "");
    assert_eq!(
        row(&c, "nightly-report"),
        sdk(State::Up, None, Some("2026-11-23T20:02:00Z"))
    );
    let want = [(
        "2026-11-23T17:02:01Z".into(),
        Some("2026-11-23T19:00:20Z".into()),
        "missed check-in".into(),
    )];
    assert_eq!(incidents(&c), want);
}

#[test]
fn a_run_that_never_ends_times_out() {
    let c = memory();
    let config = format!(r#","check_in_id":"{RUN}","monitor_config":{HOURLY}"#);
    send(
        &c,
        "2026-11-23T16:00:05Z",
        "nightly-report",
        "in_progress",
        &config,
    );
    store::sweep(&c, ms("2026-11-23T16:10:05Z")).unwrap();
    assert_eq!(row(&c, "nightly-report").state, State::Pending);
    store::sweep(&c, ms("2026-11-23T16:10:06Z")).unwrap();
    assert_eq!(row(&c, "nightly-report"), sdk(State::Down, None, None));
    let want = [(
        "2026-11-23T16:10:06Z".into(),
        None,
        "run timed out after 10 min".into(),
    )];
    assert_eq!(incidents(&c), want);
    // The late end of the run closes the incident.
    send(
        &c,
        "2026-11-23T16:20:00Z",
        "nightly-report",
        "ok",
        &format!(r#","check_in_id":"{RUN}""#),
    );
    assert_eq!(
        row(&c, "nightly-report"),
        sdk(State::Up, None, Some("2026-11-23T17:02:00Z"))
    );
    assert_eq!(incidents(&c)[0].1.as_deref(), Some("2026-11-23T16:20:00Z"));
}

#[test]
fn a_start_that_arrives_after_its_end_opens_no_run() {
    let c = memory();
    let first = format!(r#","check_in_id":"{RUN}","monitor_config":{HOURLY}"#);
    send(
        &c,
        "2026-11-23T16:00:05Z",
        "nightly-report",
        "in_progress",
        &first,
    );
    let end = format!(r#","check_in_id":"{RUN}""#);
    send(&c, "2026-11-23T16:00:06Z", "nightly-report", "ok", &end);
    let next = "2a31a644c40041b9942df6d115f2519a";
    send(
        &c,
        "2026-11-23T17:00:01Z",
        "nightly-report",
        "ok",
        &format!(r#","check_in_id":"{next}""#),
    );
    let late = format!(r#","check_in_id":"{next}","monitor_config":{HOURLY}"#);
    send(
        &c,
        "2026-11-23T17:00:02Z",
        "nightly-report",
        "in_progress",
        &late,
    );
    let waiting = sdk(State::Up, None, Some("2026-11-23T18:02:00Z"));
    assert_eq!(row(&c, "nightly-report"), waiting);
    store::sweep(&c, ms("2026-11-23T17:20:00Z")).unwrap();
    assert_eq!(row(&c, "nightly-report"), waiting);
    assert_eq!(incidents(&c), []);
    // The run after it starts as usual.
    let after = r#","check_in_id":"4c6f05a21b444656a4ae604d69d3e1f1""#;
    send(
        &c,
        "2026-11-23T18:00:00Z",
        "nightly-report",
        "in_progress",
        after,
    );
    assert_eq!(
        row(&c, "nightly-report"),
        sdk(
            State::Up,
            Some("4c6f05a21b444656a4ae604d69d3e1f1"),
            Some("2026-11-23T18:10:00Z")
        )
    );
}

#[test]
fn an_error_is_a_failure_and_a_run_ends_only_by_its_own_id() {
    let c = memory();
    let config = format!(r#","check_in_id":"{RUN}","monitor_config":{HOURLY}"#);
    send(
        &c,
        "2026-11-23T16:00:05Z",
        "nightly-report",
        "in_progress",
        &config,
    );
    // Another run's end leaves this run open, due at its deadline.
    let other = r#","check_in_id":"2a31a644c40041b9942df6d115f2519a""#;
    send(&c, "2026-11-23T16:01:00Z", "nightly-report", "error", other);
    assert_eq!(
        row(&c, "nightly-report"),
        sdk(State::Down, Some(RUN), Some("2026-11-23T16:10:05Z"))
    );
    assert_eq!(
        checks(&c, "nightly-report"),
        [(false, None, Some("failure ping".into()))]
    );
    // All zeros names the open run, which then took 115 s.
    send(
        &c,
        "2026-11-23T16:02:00Z",
        "nightly-report",
        "ok",
        r#","check_in_id":"00000000000000000000000000000000""#,
    );
    assert_eq!(
        row(&c, "nightly-report"),
        sdk(State::Up, None, Some("2026-11-23T17:02:00Z"))
    );
    assert_eq!(checks(&c, "nightly-report")[1], (true, Some(115_000), None));
    assert_eq!(incidents(&c)[0].2, "failure ping");
}

#[test]
fn refused_check_ins_are_counted_by_reason() {
    let c = memory();
    send(&c, "2026-11-23T16:00:00Z", "nightly-report", "ok", "");
    send(&c, "2026-11-23T16:00:01Z", "nightly-report", "ok", "");
    send(
        &c,
        "2026-11-23T16:00:02Z",
        "nightly-report",
        "ok",
        r#","monitor_config":{"schedule":{"type":"crontab","value":"0 0 31 2 *"}}"#,
    );
    send(&c, "2026-11-23T16:00:03Z", "bad.slug", "ok", "");
    send(&c, "2026-11-23T16:00:04Z", "nightly-report", "timeout", "");
    let big = checkin::parse(&vec![b' '; checkin::MAX_ITEM + 1]);
    apply(&c, 1, ms("2026-11-23T16:00:05Z"), &big).unwrap();
    let want = [
        ("davy_bad_config", 1),
        ("davy_bad_slug", 1),
        ("davy_invalid", 1),
        ("davy_too_large", 1),
        ("davy_unknown_monitor", 2),
    ];
    assert_eq!(dropped(&c), want.map(|(r, n)| (r.to_owned(), n)));
    let days: Vec<i64> = c
        .prepare("SELECT day FROM client_reports")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(days, [ms("2026-11-23T00:00:00Z"); 5]);
    assert_eq!(store::list(&c).unwrap().len(), 0);
}

#[test]
fn check_ins_create_at_most_100_monitors_per_project() {
    let c = memory();
    let config = r#","monitor_config":{"schedule":{"type":"interval","value":1,"unit":"hour"}}"#;
    for i in 0..=MAX_SDK_MONITORS {
        send(
            &c,
            "2026-11-23T16:00:00Z",
            &format!("job-{i}"),
            "ok",
            config,
        );
    }
    assert_eq!(store::list(&c).unwrap().len(), 100);
    assert_eq!(dropped(&c), [("davy_monitor_cap".to_owned(), 1)]);
    // The operator's room is apart from the senders'.
    let backup = NewMonitor::new(
        "Backup",
        Target::Heartbeat {
            grace: Duration::ZERO,
            slug: None,
        },
        None,
    )
    .unwrap();
    store::add(&c, 1, &backup, 0).unwrap();
    // Another project has room of its own.
    crate::store::create_project(&c, "blog").unwrap();
    let json = format!(r#"{{"monitor_slug":"job-0","status":"ok"{config}}}"#);
    apply(&c, 2, 0, &checkin::parse(json.as_bytes())).unwrap();
    assert_eq!(store::list(&c).unwrap().len(), 102);
}

#[test]
fn a_monitor_takes_six_results_a_minute() {
    let c = memory();
    let config = r#","monitor_config":{"schedule":{"type":"interval","value":1,"unit":"minute"}}"#;
    for second in 0..6 {
        send(
            &c,
            &format!("2026-11-23T16:00:{second:02}Z"),
            "busy",
            "ok",
            config,
        );
        send(
            &c,
            &format!("2026-11-23T16:00:{second:02}Z"),
            "busy",
            "in_progress",
            "",
        );
    }
    send(&c, "2026-11-23T16:00:59Z", "busy", "error", "");
    assert_eq!(dropped(&c), [("davy_rate".to_owned(), 7)]);
    assert_eq!(checks(&c, "busy").len(), 3);
    send(&c, "2026-11-23T16:01:01Z", "busy", "ok", "");
    assert_eq!(checks(&c, "busy").len(), 4);
}

#[test]
fn check_ins_never_change_a_monitor_made_from_the_cli() {
    let c = memory();
    let target = Target::Heartbeat {
        grace: Duration::from_mins(5),
        slug: MonitorSlug::parse("Backup"),
    };
    let m = NewMonitor::new("Nightly backup", target, Some(Duration::from_hours(24))).unwrap();
    let added = store::add(&c, 1, &m, 0).unwrap();
    let again = store::add(&c, 1, &m, 0).unwrap_err().to_string();
    assert_eq!(again, "the project already has a monitor with slug backup");
    let config =
        r#","monitor_config":{"schedule":{"type":"crontab","value":"* * * * *"},"max_runtime":1}"#;
    send(&c, "2026-11-23T16:00:00Z", "backup", "in_progress", config);
    send(&c, "2026-11-23T16:20:00Z", "backup", "ok", config);
    let cli = Row {
        state: State::Up,
        source: "cli".into(),
        every_s: 86_400,
        cron: None,
        grace_s: Some(300),
        max_runtime_s: None,
        run_id: None,
        due_at: Some("2026-11-24T16:25:00Z".into()),
    };
    assert_eq!(row(&c, "backup"), cli);
    // CLI starts do not open a run or supply a duration.
    assert_eq!(checks(&c, "backup"), [(true, None, None)]);
    assert!(added.target.starts_with("/api/heartbeat/"));
    // A token ping keeps the same expectation.
    let id: i64 = c
        .query_row("SELECT id FROM monitors WHERE slug='backup'", [], |r| {
            r.get(0)
        })
        .unwrap();
    store::ping(&c, id, ms("2026-11-23T17:00:00Z"), false).unwrap();
    assert_eq!(
        row(&c, "backup").due_at.as_deref(),
        Some("2026-11-24T17:05:00Z")
    );
    store::sweep(&c, ms("2026-11-24T17:05:01Z")).unwrap();
    assert_eq!(incidents(&c)[0].2, "missed check-in");
}

#[test]
fn a_heartbeat_without_a_slug_is_still_late() {
    let c = memory();
    let m = NewMonitor::new(
        "Backup",
        Target::Heartbeat {
            grace: Duration::from_mins(5),
            slug: None,
        },
        None,
    )
    .unwrap();
    let id = store::add(&c, 1, &m, 0).unwrap().id;
    store::ping(&c, id, 0, false).unwrap();
    store::sweep(&c, 3_900_000).unwrap();
    assert_eq!(incidents(&c), []);
    store::sweep(&c, 3_900_001).unwrap();
    assert_eq!(incidents(&c)[0].2, "no ping in time");
}

#[test]
fn the_schema_holds_one_schedule() {
    let c = memory();
    send(
        &c,
        "2026-11-23T16:00:00Z",
        "job",
        "ok",
        &format!(r#","monitor_config":{HOURLY}"#),
    );
    for bad in [
        "UPDATE monitors SET zone=NULL",
        "UPDATE monitors SET months=1",
        "UPDATE monitors SET every_s=60",
        "UPDATE monitors SET cron=NULL",
        "UPDATE monitors SET source='agent'",
    ] {
        assert!(c.execute(bad, params![]).is_err(), "{bad}");
    }
    c.execute("UPDATE monitors SET cron=NULL,months=3", params![])
        .unwrap();
    assert_eq!(
        store::list(&c).unwrap()[0].schedule.as_deref(),
        Some("every 3 months, Europe/Prague")
    );
}

#[test]
fn the_migration_gives_old_heartbeats_their_due_time() {
    let mut c = memory();
    let m = NewMonitor::new(
        "Backup",
        Target::Heartbeat {
            grace: Duration::from_mins(5),
            slug: None,
        },
        None,
    )
    .unwrap();
    let id = store::add(&c, 1, &m, 0).unwrap().id;
    store::ping(&c, id, ms("2026-11-23T16:00:00Z"), false).unwrap();
    c.execute_batch(&format!(
        "ALTER TABLE channels DROP COLUMN sdk_dropped; ALTER TABLE projects DROP COLUMN scrub; {} {} PRAGMA user_version=11;",
        super::checkin_store::UNDO,
        crate::retention::UNDO
    ))
    .unwrap();
    crate::store::init(&mut c).unwrap();
    let (source, due_at): (String, Option<i64>) = c
        .query_row("SELECT source,due_at FROM monitors", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(
        (source.as_str(), at(due_at).as_deref()),
        ("cli", Some("2026-11-23T17:05:00Z"))
    );
}
