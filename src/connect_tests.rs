use super::*;

#[test]
fn connect_reuses_a_project_and_prints_local_instructions() {
    let data = crate::test_support::scratch("connect");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(
        data.join(install::SETTINGS_FILE),
        r#"{"public_url":"https://davy.example/","listen":"0.0.0.0:9876","mail_from":null}"#,
    )
    .unwrap();
    let args = Args {
        slug: "api".into(),
        data: data.clone(),
        sdk: Some(Sdk::Node),
        local: true,
    };
    let mut first = Vec::new();
    run(&mut first, &args).unwrap();
    let mut second = Vec::new();
    run(&mut second, &args).unwrap();
    assert_eq!(first, second);
    let result: serde_json::Value = serde_json::from_slice(&first).unwrap();
    assert_eq!(result["project"]["id"], 1);
    assert_eq!(result["project"]["slug"], "api");
    assert_eq!(result["sdk"]["package"], "@sentry/node");
    let dsn = Url::parse(result["project"]["dsn"].as_str().unwrap()).unwrap();
    assert_eq!(dsn.host_str(), Some("127.0.0.1"));
    assert_eq!(dsn.port(), Some(9876));
    assert!(
        result["test_event"]
            .as_str()
            .unwrap()
            .contains("http://127.0.0.1:9876/api/1/store/")
    );
    let public = existing(
        &data,
        "api",
        &Url::parse("https://davy.example/").unwrap(),
        None,
    )
    .unwrap();
    assert_eq!(
        Url::parse(&public.project.dsn).unwrap().host_str(),
        Some("davy.example")
    );
    assert!(public.sdk.is_none());
    assert_eq!(
        public.supported_sdks,
        [
            "android", "rust", "go", "node", "python", "dotnet", "browser"
        ]
    );
    let mut listed = Vec::new();
    list(&mut listed, &data).unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&listed).unwrap(),
        serde_json::json!([{"id":1,"slug":"api"}])
    );
    assert!(
        existing(
            &data,
            "missing",
            &Url::parse("https://davy.example/").unwrap(),
            None
        )
        .is_err()
    );
    let mut shown =
        serde_json::to_value(status(&data, "api", retention::Defaults::default()).unwrap())
            .unwrap();
    let kept = shown.as_object_mut().unwrap().remove("retention").unwrap();
    assert_eq!(
        shown,
        serde_json::json!({"event_count":0,"last_event_time":null,"issue_count":0,"last_log_time":null,"davy":{"project":null,"warnings_24h":0,"errors_24h":0,"crashes_24h":[],"crashes_waiting":0},"scrub":{"keys":true,"credentials":true,"cards":true,"email":false,"ip":true,"personal":false,"extra_keys":[]}})
    );
    assert_eq!(kept["project"], "api");
    assert_eq!(kept["logs"]["days"], 30);
    assert_eq!(kept["logs"]["server_default"], true);
    assert_eq!(kept["errors"]["days"], 36_500);
}

#[test]
fn local_dsn_uses_the_listener_family_and_refuses_unreachable_loopback() {
    assert_eq!(
        local_url("[::]:3456").unwrap().as_str(),
        "http://[::1]:3456/"
    );
    assert_eq!(
        local_url("127.0.0.2:8765").unwrap().as_str(),
        "http://127.0.0.2:8765/"
    );
    assert!(local_url("192.0.2.1:8000").is_err());
    assert!(local_url("127.0.0.1:0").is_err());
}

#[test]
fn project_status_returns_only_the_requested_projects_latest_log() {
    use crate::logs::{LogLevel, LogRecord, LogSource, store::LogWriter};
    let data = crate::test_support::scratch("connect-status");
    let c = store::open(&data).unwrap();
    store::create_project(&c, "api").unwrap();
    store::create_project(&c, "other").unwrap();
    let log = |project, timestamp| LogRecord {
        project_id: project,
        timestamp,
        observed_at: timestamp,
        source: LogSource::Logtail,
        level: LogLevel::Info,
        level_unknown: false,
        message: "connected".into(),
        service: None,
        trace_id: None,
        span_id: None,
        attributes: serde_json::Map::new(),
    };
    let writer = LogWriter::start(logs::dir(&data), 30, logs::store::no_overrides()).unwrap();
    let at = time::now_ns() - 1_000_000_000;
    assert!(
        writer
            .queue
            .push(vec![log(1, at - 1), log(1, at), log(2, at + 1)])
            .is_ok()
    );
    drop(writer.queue);
    writer.thread.join().unwrap();
    let result = status(&data, "api", retention::Defaults::default()).unwrap();
    assert_eq!(result.last_log_time, Some(time::rfc3339(at)));
    assert_eq!(result.event_count, 0);
    assert_eq!(result.issue_count, 0);
}
