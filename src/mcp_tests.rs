use super::*;
use axum::body::{Body, to_bytes};
use axum::http::header;
use serde_json::{Value, json};
use tower::ServiceExt;

const SECRET: &str = "a-test-token-that-is-long-enough!";

struct Server {
    app: Router,
    data: PathBuf,
    writes: crossbeam_channel::Receiver<store::Write>,
}

fn server() -> Server {
    let data = crate::test_support::scratch("mcp");
    let c = store::open(&data).unwrap();
    store::create_project(&c, "api").unwrap();
    let (tx, writes) = crossbeam_channel::bounded(8);
    Server {
        data: data.clone(),
        app: router(
            data,
            tx,
            Token::new(SECRET),
            &url::Url::parse("https://davy.example/").unwrap(),
        ),
        writes,
    }
}

/// The next write that is not the first agent's.
fn next_job(writes: &crossbeam_channel::Receiver<store::Write>) -> store::Write {
    loop {
        let w = writes.recv().unwrap();
        if !matches!(w.job, Job::First { .. }) {
            return w;
        }
    }
}

async fn post(app: &Router, host: &str, token: Option<&str>, body: Value) -> (u16, String) {
    let mut req = axum::http::Request::post("/mcp")
        .header(header::HOST, host)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream");
    if let Some(t) = token {
        req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    let r = app
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = r.status().as_u16();
    let body = to_bytes(r.into_body(), 1_000_000).await.unwrap();
    (status, String::from_utf8(body.to_vec()).unwrap())
}

fn call(tool: &str, arguments: &Value) -> Value {
    json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
           "params": {"name": tool, "arguments": arguments}})
}

/// The text and `isError` flag of a tool result.
async fn tool(app: &Router, tool: &str, arguments: Value) -> (String, bool) {
    let (status, body) = post(app, "localhost", Some(SECRET), call(tool, &arguments)).await;
    assert_eq!(status, 200, "{body}");
    let reply: Value = serde_json::from_str(&body).unwrap();
    let result = &reply["result"];
    (
        result["content"][0]["text"].as_str().unwrap().to_owned(),
        result["isError"].as_bool().unwrap_or(false),
    )
}

#[tokio::test]
async fn mcp_answers_only_with_the_token() {
    let s = server();
    let list = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"});
    for (token, expected) in [(None, 401), (Some("wrong"), 401), (Some(SECRET), 200)] {
        let (status, body) = post(&s.app, "localhost", token, list.clone()).await;
        assert_eq!(status, expected, "{token:?}: {body}");
    }
}

#[tokio::test]
async fn the_first_request_with_the_token_records_an_agent_once() {
    let s = server();
    let list = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"});
    post(&s.app, "localhost", Some("wrong"), list.clone()).await;
    tokio::task::yield_now().await;
    assert!(
        s.writes.try_recv().is_err(),
        "a refused request is no agent"
    );
    for _ in 0..3 {
        post(&s.app, "localhost", Some(SECRET), list.clone()).await;
    }
    let write = s
        .writes
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    let Job::First { what, at } = write.job else {
        panic!("the write is no first agent");
    };
    assert_eq!(what, First::Agent);
    assert!(time::now_ms() - at < 60_000);
    // The runtime has one thread, so yielding lets every spawned write run.
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    assert!(s.writes.try_recv().is_err(), "one write per server start");
}

#[tokio::test]
async fn a_ui_session_cookie_is_no_token() {
    let s = server();
    let list = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"});
    let req = axum::http::Request::post("/mcp")
        .header(header::HOST, "localhost")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header(header::COOKIE, format!("__Host-davy_session={SECRET}"))
        .body(Body::from(list.to_string()))
        .unwrap();
    assert_eq!(s.app.clone().oneshot(req).await.unwrap().status(), 401);
}

#[tokio::test]
async fn mcp_answers_only_for_loopback_and_the_public_host() {
    let s = server();
    let list = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"});
    for (host, expected) in [
        ("localhost:8080", 200),
        ("127.0.0.1", 200),
        ("davy.example", 200),
        ("attacker.example", 403),
    ] {
        let (status, body) = post(&s.app, host, Some(SECRET), list.clone()).await;
        assert_eq!(status, expected, "{host}: {body}");
    }
}

#[tokio::test]
async fn mcp_refuses_pages_from_other_origins_and_an_empty_host() {
    let s = server();
    let list = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"});
    for (host, origin, expected) in [
        ("davy.example", None, 200),
        ("davy.example", Some("https://davy.example"), 200),
        ("localhost", Some("http://localhost:6274"), 200),
        ("davy.example", Some("https://attacker.example"), 403),
        ("davy.example", Some("http://davy.example"), 403),
        ("davy.example", Some("null"), 403),
        ("", None, 400),
    ] {
        let mut req = axum::http::Request::post("/mcp")
            .header(header::HOST, host)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json, text/event-stream")
            .header(header::AUTHORIZATION, format!("Bearer {SECRET}"));
        if let Some(origin) = origin {
            req = req.header(header::ORIGIN, origin);
        }
        let req = req.body(Body::from(list.to_string())).unwrap();
        let status = s.app.clone().oneshot(req).await.unwrap().status();
        assert_eq!(status.as_u16(), expected, "{host} {origin:?}");
    }
}

#[tokio::test]
async fn sql_tool_reads_and_refuses_writes() {
    let s = server();
    assert_eq!(
        tool(&s.app, "sql", json!({"query": "SELECT slug FROM projects"})).await,
        (
            r#"{"columns":["slug"],"rows":[["api"]],"truncated":false}"#.to_owned(),
            false
        )
    );
    let (text, is_error) = tool(&s.app, "sql", json!({"query": "DELETE FROM projects"})).await;
    assert!(is_error, "{text}");
}

#[tokio::test]
async fn mark_fixed_queues_a_resolve_and_reports_the_writer_outcome() {
    let s = server();
    let writer = std::thread::spawn(move || {
        for outcome in [Ok(()), Err("no issue 9".to_owned())] {
            let w = next_job(&s.writes);
            let Job::SetStatus {
                issues,
                status: IssueStatus::Resolved { in_release, .. },
            } = w.job
            else {
                panic!("expected a resolve job");
            };
            w.done.send(outcome).unwrap();
            assert_eq!(in_release.as_deref(), Some("2.0.0"));
            assert!(issues == IssueIds::one(7) || issues == IssueIds::one(9));
        }
    });
    assert_eq!(
        tool(
            &s.app,
            "mark_fixed",
            json!({"issue_id": 7, "release": "2.0.0"})
        )
        .await,
        ("issue 7 resolved in release 2.0.0".to_owned(), false)
    );
    assert_eq!(
        tool(
            &s.app,
            "mark_fixed",
            json!({"issue_id": 9, "release": "2.0.0"})
        )
        .await,
        ("no issue 9".to_owned(), true)
    );
    writer.join().unwrap();
}

#[tokio::test]
async fn submit_feedback_queues_the_text_with_the_client_name() {
    let s = server();
    let writer = std::thread::spawn(move || {
        let mut seen = Vec::new();
        for _ in 0..2 {
            let w = next_job(&s.writes);
            let Job::Feedback { feedback, .. } = w.job else {
                panic!("expected a feedback job");
            };
            seen.push(feedback);
            w.done.send(Ok(())).unwrap();
        }
        seen
    });
    let thanks = (
        "Thanks. The feedback is stored for the person who runs Davy.".to_owned(),
        false,
    );
    let mut with_meta = call(
        "submit_feedback",
        &json!({"tried": "sql on logs", "happened": "no such table: logs", "fix": "say logs are not in SQL"}),
    );
    with_meta["params"]["_meta"] = json!({
        "io.modelcontextprotocol/clientInfo": {"name": "claude-code", "version": "2.1.0"}
    });
    let (status, body) = post(&s.app, "localhost", Some(SECRET), with_meta).await;
    assert_eq!(status, 200, "{body}");
    let reply: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(reply["result"]["content"][0]["text"], thanks.0.as_str());
    let request = axum::http::Request::post("/mcp")
        .header(header::HOST, "localhost")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header(header::AUTHORIZATION, format!("Bearer {SECRET}"))
        .header(header::USER_AGENT, "codex_cli_rs/0.80.0")
        .body(Body::from(
            call(
                "submit_feedback",
                &json!({"tried": "issue_context 3", "happened": "no source lines"}),
            )
            .to_string(),
        ))
        .unwrap();
    let r = s.app.clone().oneshot(request).await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(
        tool(
            &s.app,
            "submit_feedback",
            json!({"tried": "", "happened": "x"})
        )
        .await,
        ("tried is empty".to_owned(), true)
    );
    assert_eq!(
        writer.join().unwrap(),
        [
            Feedback {
                client: Some("claude-code".to_owned()),
                tried: "sql on logs".to_owned(),
                happened: "no such table: logs".to_owned(),
                fix: Some("say logs are not in SQL".to_owned()),
            },
            Feedback {
                client: Some("codex_cli_rs/0.80.0".to_owned()),
                tried: "issue_context 3".to_owned(),
                happened: "no source lines".to_owned(),
                fix: None,
            },
        ]
    );
}

#[tokio::test]
async fn count_logs_counts_per_bucket_with_the_filters_of_search_logs() {
    use crate::logs::{LogRecord, LogSource, store::LogWriter};
    const MIN: i64 = 60_000_000_000;
    let s = server();
    let now = time::now_ns();
    let base = now - now % (60 * MIN) - 120 * MIN;
    let log = |at: i64, level: LogLevel, message: &str| LogRecord {
        project_id: 1,
        timestamp: at,
        observed_at: now,
        source: LogSource::Logtail,
        level,
        message: message.into(),
        service: None,
        trace_id: None,
        span_id: None,
        attributes: serde_json::Map::new(),
    };
    let writer = LogWriter::start(logs::dir(&s.data), 30).unwrap();
    let batch = vec![
        log(base + 10 * MIN, LogLevel::Error, "database timeout"),
        log(base + 10 * MIN + 1, LogLevel::Warn, "database timeout"),
        log(base + 70 * MIN, LogLevel::Error, "database timeout"),
        log(base + 71 * MIN, LogLevel::Info, "request completed"),
    ];
    assert!(writer.queue.push(batch).is_ok());
    drop(writer.queue);
    writer.thread.join().unwrap();
    let window =
        json!({"since": time::rfc3339(base), "until": time::rfc3339(base + 120 * MIN - 1)});
    let with = |extra: Value| {
        let mut args = window.clone();
        args.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        args
    };
    let (text, is_error) = tool(&s.app, "count_logs", with(json!({"query": "timeout"}))).await;
    assert!(!is_error, "{text}");
    assert_eq!(
        serde_json::from_str::<Value>(&text).unwrap(),
        json!({"total": 3, "step": "1m", "buckets": [
            [time::rfc3339(base + 10 * MIN), 2],
            [time::rfc3339(base + 70 * MIN), 1],
        ]})
    );
    let (text, _) = tool(
        &s.app,
        "count_logs",
        with(json!({"level": "error", "project": "api"})),
    )
    .await;
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap()["total"], 2);
    let (text, is_error) = tool(
        &s.app,
        "search_logs",
        with(json!({"query": "timeout", "limit": 1})),
    )
    .await;
    assert!(!is_error, "{text}");
    let hits: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(hits.as_array().unwrap().len(), 1);
    assert_eq!(hits[0]["timestamp_ns"], base + 70 * MIN);
    let (text, is_error) = tool(&s.app, "count_logs", json!({"level": "loud"})).await;
    assert!(is_error, "{text}");
}

#[tokio::test]
async fn both_log_tools_show_every_filter_in_their_schema() {
    let s = server();
    let list = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"});
    let (_, body) = post(&s.app, "localhost", Some(SECRET), list).await;
    let reply: Value = serde_json::from_str(&body).unwrap();
    let fields = |name: &str| {
        let tools = reply["result"]["tools"].as_array().unwrap();
        let t = tools.iter().find(|t| t["name"] == name).unwrap();
        let mut keys: Vec<String> = t["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        keys.sort();
        keys
    };
    assert_eq!(
        fields("search_logs"),
        ["level", "limit", "project", "query", "since", "until"]
    );
    assert_eq!(
        fields("count_logs"),
        ["level", "project", "query", "since", "until"]
    );
}

#[tokio::test]
async fn release_health_reads_the_sessions_of_one_project() {
    let s = server();
    let at = time::now_ms();
    let c = store::open(&s.data).unwrap();
    let crashed = crate::items::session::Session {
        sid: [1; 16],
        started: at,
        flags: 2 | 4,
        user: None,
        release: "api@2".to_owned(),
        environment: "production".to_owned(),
    };
    crate::items::session::add_session(&c, 1, at, &crashed).unwrap();
    let (text, is_error) = tool(&s.app, "release_health", json!({"project": "api"})).await;
    assert!(!is_error, "{text}");
    let rows: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        (
            &rows[0]["release"],
            &rows[0]["sessions"],
            &rows[0]["crash_free_sessions"]
        ),
        (&json!("api@2"), &json!(1), &json!(0.0))
    );
    let (text, is_error) = tool(&s.app, "release_health", json!({"since": "40d"})).await;
    let every: Value = serde_json::from_str(&text).unwrap();
    assert_eq!((every.as_array().unwrap().len(), is_error), (1, false));
    let (text, is_error) = tool(&s.app, "release_health", json!({"project": "nope"})).await;
    assert!(is_error, "{text}");
}
