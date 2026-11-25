use super::*;
use crate::{
    logs::{LogRecord, LogSource, store::LogWriter},
    self_report::forward::find_or_create,
    test_support::scratch,
};

const MINUTE_MS: i64 = 60_000;
const ID: &str = "0123456789abcdef0123456789abcdef";

fn line(project: i64, level: LogLevel, at_ms: i64) -> LogRecord {
    LogRecord {
        project_id: project,
        timestamp: at_ms * 1_000_000,
        observed_at: at_ms * 1_000_000,
        source: LogSource::Davy,
        level,
        message: "log writer: retention of 2020-01-01 was skipped".into(),
        service: Some("davy".into()),
        trace_id: None,
        span_id: None,
        attributes: serde_json::Map::new(),
    }
}

fn wait(data: &Path) {
    let dir = crash::dir(data);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("1767225600000-{ID}.json")), "{}").unwrap();
}

#[test]
fn without_the_project_only_waiting_crashes_count() {
    let data = scratch("self-health-none");
    let c = store::open(&data).unwrap();
    let quiet = health(&c, &data, time::now_ms()).unwrap();
    assert_eq!(
        quiet,
        Health {
            warnings_24h: 0,
            errors_24h: 0,
            crashes_24h: Vec::new(),
            crashes_waiting: 0,
        }
    );
    assert!(quiet.quiet());
    wait(&data);
    let waiting = health(&c, &data, time::now_ms()).unwrap();
    assert_eq!(waiting.crashes_waiting, 1);
    assert!(!waiting.quiet());
}

#[test]
fn the_last_day_of_davys_lines_and_crashes_is_counted() {
    let data = scratch("self-health");
    let mut c = store::open(&data).unwrap();
    let (other, _) = store::create_project(&c, "api").unwrap();
    let (davy, created) = find_or_create(&mut c).unwrap();
    assert!(created);
    let now = time::now_ms();
    let writer = LogWriter::start(logs::dir(&data), 30, logs::store::no_overrides()).unwrap();
    let lines = vec![
        line(davy, LogLevel::Info, now - MINUTE_MS),
        line(davy, LogLevel::Warn, now - 2 * MINUTE_MS),
        line(davy, LogLevel::Warn, now - 3 * MINUTE_MS),
        line(davy, LogLevel::Error, now - 4 * MINUTE_MS),
        line(davy, LogLevel::Fatal, now - 5 * MINUTE_MS),
        line(davy, LogLevel::Error, now - WINDOW_MS - MINUTE_MS),
        line(other, LogLevel::Error, now - MINUTE_MS),
    ];
    assert!(writer.queue.push(lines).is_ok());
    drop(writer.queue);
    writer.thread.join().unwrap();
    let seen = |n: i64| now - n * MINUTE_MS;
    c.execute_batch(&format!(
        "INSERT INTO issues(id,project_id,grouping_version,fingerprint,title,level,status,first_seen,last_seen,event_count) VALUES
         (1,{davy},1,'a','panic: older','fatal','unresolved',0,{},1),
         (2,{davy},1,'b','panic: index out of bounds','fatal','unresolved',0,{},3),
         (3,{davy},1,'c','panic: newest','fatal','unresolved',0,{},1),
         (4,{davy},1,'d','not a crash','error','unresolved',0,{},1),
         (5,{other},1,'e','panic: of another project','fatal','unresolved',0,{},1);",
        now - WINDOW_MS - MINUTE_MS,
        seen(30),
        seen(10),
        seen(1),
        seen(1),
    ))
    .unwrap();
    wait(&data);
    assert_eq!(
        health(&c, &data, now).unwrap(),
        Health {
            warnings_24h: 2,
            errors_24h: 2,
            crashes_24h: vec![
                Crash {
                    issue_id: 3,
                    title: "panic: newest".into(),
                    last_seen: time::rfc3339_ms(seen(10)),
                    recorded_at: time::rfc3339_ms(seen(10)),
                    event_count: 1,
                },
                Crash {
                    issue_id: 2,
                    title: "panic: index out of bounds".into(),
                    last_seen: time::rfc3339_ms(seen(30)),
                    recorded_at: time::rfc3339_ms(seen(30)),
                    event_count: 3,
                },
            ],
            crashes_waiting: 1,
        }
    );
}

#[test]
fn health_reads_as_json_with_these_names() {
    let h = Health {
        warnings_24h: 1,
        errors_24h: 0,
        crashes_24h: vec![Crash {
            issue_id: 9,
            title: "panic: boom".into(),
            last_seen: "2026-01-01T00:00:00.000Z".into(),
            recorded_at: "2026-01-03T00:00:00.000Z".into(),
            event_count: 2,
        }],
        crashes_waiting: 0,
    };
    assert_eq!(
        serde_json::to_value(&h).unwrap(),
        serde_json::json!({"warnings_24h":1,"errors_24h":0,"crashes_24h":[{"issue_id":9,"title":"panic: boom","last_seen":"2026-01-01T00:00:00.000Z","recorded_at":"2026-01-03T00:00:00.000Z","event_count":2}],"crashes_waiting":0})
    );
}

#[test]
fn a_crash_recorded_today_counts_though_it_happened_days_ago() {
    let data = scratch("self-health-late");
    let mut c = store::open(&data).unwrap();
    let (davy, _) = find_or_create(&mut c).unwrap();
    let now = time::now_ms();
    let panicked = now - 3 * WINDOW_MS;
    c.execute_batch(&format!(
        "INSERT INTO issues(id,project_id,grouping_version,fingerprint,title,level,status,first_seen,last_seen,event_count) VALUES
         (1,{p},1,'a','panic: before a stop','fatal','unresolved',{panicked},{panicked},1),
         (2,{p},1,'b','panic: long gone','fatal','unresolved',{panicked},{panicked},1);
         INSERT INTO events(project_id,issue_id,event_id,timestamp,received_at,level) VALUES
         ({p},1,'{ID}',{panicked},{recorded},'fatal'),
         ({p},2,'fedcba9876543210fedcba9876543210',{panicked},{panicked},'fatal');",
        p = davy,
        recorded = now - MINUTE_MS,
    ))
    .unwrap();
    let h = health(&c, &data, now).unwrap();
    assert_eq!(
        h.crashes_24h,
        vec![Crash {
            issue_id: 1,
            title: "panic: before a stop".into(),
            last_seen: time::rfc3339_ms(panicked),
            recorded_at: time::rfc3339_ms(now - MINUTE_MS),
            event_count: 1,
        }]
    );
    drop(c);
    std::fs::remove_dir_all(&data).unwrap();
}

#[test]
fn a_health_that_cannot_be_read_says_why_without_failing() {
    let data = scratch("self-health-unread");
    let c = store::open(&data).unwrap();
    std::fs::write(crash::dir(&data), "not a directory").unwrap();
    let Davy::Unread { error } = Davy::read(&c, &data, time::now_ms()) else {
        panic!("read a crash directory that is a file");
    };
    assert!(error.contains("Not a directory"), "{error}");
    assert_eq!(
        serde_json::to_value(Davy::Unread {
            error: "boom".into()
        })
        .unwrap(),
        serde_json::json!({"error":"boom"})
    );
    drop(c);
    std::fs::remove_dir_all(&data).unwrap();
}
