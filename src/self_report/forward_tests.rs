use super::*;
use crate::{
    logs::LogLevel,
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
    let data = scratch(name);
    let mut c = store::open(&data).unwrap();
    let (own, _) = own::find_or_create(&mut c).unwrap();
    let forwarder = Forwarder {
        data,
        own,
        db: Mutex::new(c),
        logs,
        usage: usage.clone(),
        policies: Arc::default(),
        sender: crossbeam_channel::bounded(1).0,
        budget: Budget::new(1 << 20),
    };
    (forwarder, inbox, usage)
}

#[test]
fn each_line_becomes_a_record_scrubbed_with_the_projects_policy() {
    let (forwarder, inbox, usage) = forwarder("self-report-forward");
    let data = forwarder.data.clone();
    let forwarder_id = forwarder.own.id;
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
    assert_eq!(messages, [warned, "mail to [Filtered: email] failed"]);
    let id = forwarder_id;
    assert_eq!(
        batch[0],
        LogRecord {
            project_id: id,
            timestamp: AT,
            observed_at: batch[0].observed_at,
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
    assert_eq!(batch[1].level, LogLevel::Error);
    let rows = usage.take();
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].project, rows[0].signal), (id, Signal::Logs));
    assert_eq!(rows[0].count.records, 2);
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
