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

/// Posts a `tools/call` of `tool` to `/mcp` and returns the text of its
/// result.
async fn mcp_tool(app: Router, tool: &str, arguments: serde_json::Value) -> String {
    use tower::ServiceExt;
    let call = serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": tool, "arguments": arguments}});
    let req = axum::http::Request::post("/mcp")
        .header("host", "localhost")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("authorization", format!("Bearer {MCP_TOKEN}"))
        .body(axum::body::Body::from(call.to_string()))
        .unwrap();
    let reply = app.oneshot(req).await.unwrap();
    assert_eq!(reply.status(), 200);
    let body = axum::body::to_bytes(reply.into_body(), 1 << 20)
        .await
        .unwrap();
    let reply: serde_json::Value = serde_json::from_slice(&body).unwrap();
    reply["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_owned()
}

const MCP_TOKEN: &str = "a-test-token-that-is-long-enough!";

/// The join of the log writer blocks a worker, so the test runs on two.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[expect(
    clippy::await_holding_lock,
    reason = "the lock keeps other tests' sinks out for the whole test, and no task of this runtime takes it"
)]
async fn a_warning_of_davy_is_found_with_search_logs_in_the_project_davy() {
    let _lock = self_report::install_lock();
    let data = scratch("serve-self-report");
    let early = Early::install();
    self_report::warn("self report test", "a line from before the writers start");
    let log_writer = LogWriter::start(logs::dir(&data), 30, logs::store::no_overrides()).unwrap();
    let (sender, _writes) = crossbeam_channel::bounded(8);
    let app = App {
        keys: Arc::new(Keys::new(store::open(&data).unwrap())),
        sender: sender.clone(),
        logs: log_writer.queue.clone(),
        sourcemaps: Arc::new(sourcemaps::Store::open(&data)),
        budget: body::Budget::new(body::MEMORY),
        usage: Counter::default(),
    };
    let reporter = early.start(&data, &app);
    self_report::warn("self report test", "the disk under the log store is slow");
    reporter.stop().await;
    let usage = app.usage.clone();
    drop(app);
    drop(log_writer.queue);
    tokio::task::spawn_blocking(move || log_writer.thread.join().unwrap())
        .await
        .unwrap();

    let mcp = mcp::router(
        data.clone(),
        sender,
        Token::new(MCP_TOKEN),
        &url::Url::parse("https://davy.example/").unwrap(),
        crate::retention::Defaults::default(),
    );
    let early = mcp_tool(
        mcp.clone(),
        "search_logs",
        serde_json::json!({"project": "davy", "query": "\"before the writers start\"", "level": "warn"}),
    )
    .await;
    let early: serde_json::Value = serde_json::from_str(&early).unwrap();
    assert_eq!(
        early[0]["message"], "a line from before the writers start",
        "{early}"
    );
    let found = mcp_tool(
        mcp,
        "search_logs",
        serde_json::json!({"project": "davy", "query": "\"log store is slow\"", "level": "warn"}),
    )
    .await;
    let hits: serde_json::Value = serde_json::from_str(&found).unwrap();
    assert_eq!(hits.as_array().map(Vec::len), Some(1), "{found}");
    let hit = &hits[0];
    assert_eq!(
        hit["message"], "the disk under the log store is slow",
        "{found}"
    );
    assert_eq!(hit["level"], "warn");
    assert_eq!(hit["service"], "davy");
    assert_eq!(hit["source"], "davy");
    assert_eq!(hit["attributes"]["component"], "self report test");
    let id = store::project_id(&store::open(&data).unwrap(), "davy")
        .unwrap()
        .unwrap();
    assert!(
        usage
            .take()
            .iter()
            .any(|r| r.project == id && r.signal == Signal::Logs)
    );
    std::fs::remove_dir_all(&data).unwrap();
}
