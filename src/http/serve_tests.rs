use super::*;
use crate::logs::{LogLevel, LogRecord, LogSource};
use crate::test_support::scratch;
use crate::usage::{Count, Counter, Signal};
use rusqlite::Connection;

fn record(project_id: i64, at: i64) -> LogRecord {
    LogRecord {
        project_id,
        timestamp: at,
        observed_at: at,
        source: LogSource::Logtail,
        level: LogLevel::Info,
        message: "payment declined".into(),
        service: None,
        trace_id: None,
        span_id: None,
        attributes: serde_json::Map::new(),
    }
}

fn number(c: &Connection, sql: &str) -> i64 {
    c.query_row(sql, [], |r| r.get(0)).unwrap()
}

/// The join of the log writer blocks a worker, so the test reads SQLite on
/// another one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn counts_are_stored_before_the_log_writer_stops_and_its_own_after() {
    let data = scratch("serve-shut-down");
    let c = store::open(&data).unwrap();
    store::create_project(&c, "api").unwrap();
    let writer = store::Writer::start(store::open(&data).unwrap(), 16);
    let log_writer = LogWriter::start(logs::dir(&data), 30, logs::store::no_overrides()).unwrap();
    let now = crate::time::now_ns();
    assert!(log_writer.queue.push(vec![record(1, now)]).is_ok());
    // While this is held the log writer keeps running, as a slow stop does.
    let held = log_writer.queue.clone();
    let usage = Counter::default();
    usage.add(1, Signal::Logs, Count::new(1, 16));
    let stopping = tokio::spawn({
        let usage = usage.clone();
        async move { shut_down(log_writer, &usage, writer).await }
    });
    tokio::time::timeout(Duration::from_secs(30), async {
        while number(&c, "SELECT count(*) FROM usage_daily") == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the counts are stored while the log writer still runs");
    assert!(!stopping.is_finished());
    drop(held);
    stopping.await.unwrap().unwrap();
    assert_eq!(
        number(&c, "SELECT coalesce(sum(records), 0) FROM usage_daily"),
        1,
        "counted once"
    );
    assert_eq!(
        number(
            &c,
            "SELECT coalesce(sum(records), 0) FROM log_days WHERE cut = 0"
        ),
        1,
        "the log writer's last count goes out after it stopped"
    );
    drop(c);
    std::fs::remove_dir_all(&data).unwrap();
}
