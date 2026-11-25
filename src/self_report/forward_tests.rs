use super::*;
use crate::{
    logs::store::{LogInbox, LogQueue},
    self_report::Sink,
    test_support::scratch,
};
use serde_json::json;

/// 2026-01-01T00:00:00Z.
const AT: i64 = 1_767_225_600_000_000_000;

fn forwarder(name: &str) -> (Forwarder, LogInbox, usage::Counter) {
    let (logs, inbox) = LogQueue::bounded(1000, 1 << 20);
    let usage = usage::Counter::default();
    let forwarder = Forwarder {
        data: scratch(name),
        logs,
        usage: usage.clone(),
        policies: Arc::default(),
        sender: crossbeam_channel::bounded(1).0,
        budget: Budget::new(1 << 20),
    };
    (forwarder, inbox, usage)
}

fn number(c: &Connection, sql: &str) -> Option<i64> {
    c.query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn the_first_line_makes_the_project_and_each_line_becomes_a_scrubbed_record() {
    let (forwarder, inbox, usage) = forwarder("self-report-forward");
    let data = forwarder.data.clone();
    let (sink, lines) = Sink::new(8);
    let warned = "retention of 2026-01-01, skipped until the next pass: disk full";
    sink.offer(LogLevel::Warn, "log writer", warned, AT);
    sink.offer(
        LogLevel::Error,
        "notifier",
        "mail to ops@example.com failed",
        AT,
    );
    drop(sink);
    forwarder.start(lines).unwrap().join().unwrap();

    let batch = inbox.try_recv().unwrap();
    let messages: Vec<&str> = batch.iter().map(|r| r.message.as_str()).collect();
    assert_eq!(
        messages,
        [
            "created project \"davy\" for Davy's own warnings, errors and crashes, keeping 30 days of logs; `davy serve --no-self-report` turns this off",
            warned,
            "mail to [Filtered: email] failed",
        ]
    );
    let c = store::open(&data).unwrap();
    let id = store::project_id(&c, "davy").unwrap().unwrap();
    assert_eq!(
        batch[1],
        LogRecord {
            project_id: id,
            timestamp: AT,
            observed_at: batch[1].observed_at,
            source: LogSource::Davy,
            level: LogLevel::Warn,
            message: warned.to_owned(),
            service: Some("davy".to_owned()),
            trace_id: None,
            span_id: None,
            attributes:
                json!({"component": "log writer", "davy.version": env!("CARGO_PKG_VERSION")})
                    .as_object()
                    .unwrap()
                    .clone(),
        }
    );
    assert_eq!(batch[0].level, LogLevel::Info);
    let rows = usage.take();
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].project, rows[0].signal), (id, Signal::Logs));
    assert_eq!(rows[0].count.records, 3);
    assert_eq!(
        number(
            &c,
            "SELECT log_retention_days FROM projects WHERE slug='davy'"
        ),
        Some(30)
    );
    assert_eq!(scrub::settings::read(&c, id).unwrap(), policy());
    drop(c);
    std::fs::remove_dir_all(&data).unwrap();
}

#[test]
fn a_project_davy_a_person_made_keeps_its_settings() {
    let data = scratch("self-report-own-project");
    let mut c = store::open(&data).unwrap();
    let (made, _) = store::create_project(&c, "davy").unwrap();
    assert_eq!(find_or_create(&mut c).unwrap(), (made, false));
    assert_eq!(
        number(
            &c,
            "SELECT log_retention_days FROM projects WHERE slug='davy'"
        ),
        None
    );
    assert_eq!(scrub::settings::read(&c, made).unwrap(), Policy::default());
    drop(c);
    std::fs::remove_dir_all(&data).unwrap();
}

#[test]
fn the_project_is_made_once() {
    let data = scratch("self-report-once");
    let mut c = store::open(&data).unwrap();
    let (id, created) = find_or_create(&mut c).unwrap();
    assert!(created);
    assert_eq!(find_or_create(&mut c).unwrap(), (id, false));
    drop(c);
    std::fs::remove_dir_all(&data).unwrap();
}

#[test]
fn a_record_counts_what_was_suppressed_and_dropped() {
    let line = Line {
        at_ns: AT,
        level: LogLevel::Error,
        component: "ui",
        message: "reading issues: database is locked".to_owned(),
        suppressed: 4,
        dropped: 2,
    };
    let r = record(9, line, AT + 5);
    assert_eq!((r.project_id, r.timestamp, r.observed_at), (9, AT, AT + 5));
    assert_eq!(
        Value::Object(r.attributes),
        json!({
            "component": "ui",
            "davy.version": env!("CARGO_PKG_VERSION"),
            "davy.suppressed": 4,
            "davy.dropped": 2,
        })
    );
}

#[test]
fn a_store_that_cannot_be_opened_loses_the_lines_and_not_the_thread() {
    let (mut forwarder, inbox, usage) = forwarder("self-report-no-store");
    let file = forwarder.data.clone();
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "not a directory").unwrap();
    forwarder.data = file.clone();
    let (sink, lines) = Sink::new(8);
    sink.offer(LogLevel::Warn, "sweep", "first", AT);
    drop(sink);
    forwarder.start(lines).unwrap().join().unwrap();
    assert!(inbox.try_recv().is_none());
    assert_eq!(usage.take(), []);
    std::fs::remove_file(&file).unwrap();
}
