use super::*;

#[test]
fn every_command_and_flag_says_what_it_is() {
    fn missing(c: &clap::Command, path: &str, out: &mut Vec<String>) {
        for s in c.get_subcommands() {
            let path = format!("{path} {}", s.get_name());
            if s.get_about().is_none() {
                out.push(path.clone());
            }
            for a in s.get_arguments() {
                if a.get_help().is_none() {
                    out.push(format!("{path} {}", a.get_id()));
                }
            }
            missing(s, &path, out);
        }
    }
    let mut out = vec![];
    missing(&cli(), "davy", &mut out);
    assert_eq!(out, Vec::<String>::new());
}

#[test]
fn the_admin_token_is_not_a_command_line_flag() {
    let serve = |extra: &[&str]| {
        let base = ["davy", "serve", "--data", "d", "--listen", "127.0.0.1:0"];
        let url = ["--public-url", "http://127.0.0.1/"];
        Cli::try_parse_from(base.iter().chain(&url).chain(extra)).is_ok()
    };
    assert!(serve(&[]));
    assert!(!serve(&["--token", "0123456789abcdef0123456789abcdef"]));
}

#[test]
fn monitor_add_takes_the_flags_of_one_kind() {
    let add = |extra: &[&str]| {
        let base = ["davy", "monitor", "add", "shop", "API", "--data", "d"];
        Cli::try_parse_from(base.iter().chain(extra)).is_ok()
    };
    assert!(add(&["--url", "https://shop.example/", "--timeout", "5s"]));
    assert!(add(&["--heartbeat", "--grace", "1m"]));
    for bad in [
        &[][..],
        &["--url", "https://shop.example/", "--heartbeat"],
        &["--heartbeat", "--timeout", "5s"],
        &["--heartbeat", "--keyword", "ok"],
        &["--url", "https://shop.example/", "--grace", "1m"],
        &[
            "--url",
            "https://shop.example/",
            "--keyword",
            "a",
            "--no-keyword",
            "b",
        ],
    ] {
        assert!(!add(bad), "{bad:?}");
    }
}

#[test]
fn monitor_edit_takes_at_least_one_change() {
    let edit = |extra: &[&str]| {
        let base = ["davy", "monitor", "edit", "3", "--data", "d"];
        Cli::try_parse_from(base.iter().chain(extra)).is_ok()
    };
    assert!(edit(&["--name", "API", "--project", "blog"]));
    assert!(edit(&["--every", "2h", "--grace", "0s", "--no-slug"]));
    assert!(edit(&["--status-only", "--timeout", "5s", "--json"]));
    for bad in [
        &[][..],
        &["--json"],
        &["--keyword", "a", "--no-keyword", "b"],
        &["--keyword", "a", "--status-only"],
        &["--slug", "backup", "--no-slug"],
        &["--every", "soon"],
    ] {
        assert!(!edit(bad), "{bad:?}");
    }
}

#[test]
fn channel_add_takes_a_project_or_all() {
    let add = |extra: &[&str]| {
        let base = ["davy", "channel", "add", "--data", "d"];
        Cli::try_parse_from(base.iter().chain(extra)).is_ok()
    };
    assert!(add(&[
        "webhook",
        "https://hooks.example/",
        "--project",
        "shop"
    ]));
    assert!(add(&["slack", "https://hooks.slack.com/", "--all"]));
    assert!(add(&["email", "ops@example.com", "--all"]));
    for bad in [
        &["webhook", "https://hooks.example/"][..],
        &[
            "webhook",
            "https://hooks.example/",
            "--project",
            "shop",
            "--all",
        ],
        &["sms", "+420123456789", "--all"],
    ] {
        assert!(!add(bad), "{bad:?}");
    }
}

#[test]
fn sql_text_output_separates_columns_with_tabs() {
    let mut out = Vec::new();
    let rows = query::Rows {
        columns: vec!["id".into(), "na\u{1b}me".into()],
        rows: vec![vec![serde_json::json!(1), serde_json::json!("API\t")]],
        truncated: false,
    };
    print_rows(&mut out, &rows, false).unwrap();
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "id\tna\u{fffd}me\n1\tAPI\u{fffd}\n"
    );
}

#[test]
fn text_output_replaces_control_characters() {
    let mut out = Vec::new();
    let issue = store::Issue {
        id: 1,
        status: "unresolved".into(),
        count: 2,
        title: "Error: \u{1b}]52;c;cHduZWQ=\u{7}".into(),
        culprit: Some("a\tb\u{9b}2J".into()),
    };
    print_issues(&mut out, vec![issue], false).unwrap();
    let hit = LogHit {
        timestamp: "2026-11-20T10:00:00Z".into(),
        timestamp_ns: 0,
        project_id: 1,
        source: logs::LogSource::Logtail,
        level: LogLevel::Info,
        service: Some("api\r".into()),
        message: "line\n\u{1b}[2Jgone".into(),
        trace_id: None,
        span_id: None,
        attributes: serde_json::json!({"k\u{1b}": "v\u{7}"})
            .as_object()
            .unwrap()
            .clone(),
    };
    print_logs(&mut out, vec![hit], false).unwrap();
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "1\tunresolved\t2\tError: \u{fffd}]52;c;cHduZWQ=\u{fffd}\ta\u{fffd}b\u{fffd}2J\n\
         2026-11-20T10:00:00Z info  api\u{fffd} line\u{fffd}\u{fffd}[2Jgone k\u{fffd}=v\u{fffd}\n"
    );
}

#[test]
fn logs_count_prints_the_buckets_that_have_logs() {
    use davy::logs::{LogRecord, LogSource, store::LogWriter};
    const MIN: i64 = 60_000_000_000;
    let data = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join(format!("test-cli-count-{}", uuid::Uuid::new_v4()));
    store::create_project(&store::open(&data).unwrap(), "api").unwrap();
    let now = now_ns();
    let base = now - now % (60 * MIN) - 120 * MIN;
    let log = |at: i64, message: &str| LogRecord {
        project_id: 1,
        timestamp: at,
        observed_at: now,
        source: LogSource::Logtail,
        level: LogLevel::Error,
        message: message.into(),
        service: None,
        trace_id: None,
        span_id: None,
        attributes: serde_json::Map::new(),
    };
    let writer = LogWriter::start(logs::dir(&data), 30, logs::store::no_overrides()).unwrap();
    let batch = vec![
        log(base + 10 * MIN, "database timeout"),
        log(base + 10 * MIN + 1, "database timeout"),
        log(base + 70 * MIN, "database timeout"),
        log(base + 71 * MIN, "request completed"),
    ];
    assert!(writer.queue.push(batch).is_ok());
    drop(writer.queue);
    writer.thread.join().unwrap();
    let run = |extra: &[&str]| {
        let since = time::rfc3339(base);
        let until = time::rfc3339(base + 120 * MIN - 1);
        let d = data.to_str().unwrap();
        let base_args = ["davy", "logs", "timeout", "--data", d, "--count"];
        let window = ["--since", &since, "--until", &until];
        let args = base_args.iter().chain(&window).chain(extra);
        let Command::Logs(args) = Cli::try_parse_from(args).unwrap().command else {
            panic!("not the logs command");
        };
        let mut out = vec![];
        logs(&mut out, args).unwrap();
        String::from_utf8(out).unwrap()
    };
    let (a, b) = (
        time::rfc3339(base + 10 * MIN),
        time::rfc3339(base + 70 * MIN),
    );
    assert_eq!(run(&[]), format!("3 logs, 1m buckets\n{a}\t2\n{b}\t1\n"));
    assert_eq!(
        run(&["--json"]),
        format!("{{\"total\":3,\"step\":\"1m\",\"buckets\":[[\"{a}\",2],[\"{b}\",1]]}}\n")
    );
    assert!(
        Cli::try_parse_from(["davy", "logs", "--data", "d", "--count", "--limit", "5"]).is_err()
    );
    std::fs::remove_dir_all(&data).unwrap();
}

#[test]
fn slug_help_names_the_public_dsn_authority() {
    let help = Cli::command()
        .find_subcommand_mut("monitor")
        .unwrap()
        .find_subcommand_mut("add")
        .unwrap()
        .render_long_help()
        .to_string();
    assert!(help.contains("Anyone with the"));
    assert!(help.contains("public project DSN can ping a guessable slug"));
    assert!(help.contains("token-only access"));
}

#[test]
fn project_retention_waits_for_a_log_deletion_under_way() {
    let data = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join(format!("test-cli-retention-lock-{}", uuid::Uuid::new_v4()));
    store::create_project(&store::open(&data).unwrap(), "api").unwrap();
    // The log writer of a server on the same data holds the lock.
    let lock = retention::Lock::default().with_file(&data);
    let held = lock.hold_all().unwrap();
    let d = data.to_str().unwrap().to_owned();
    let (done, changed) = std::sync::mpsc::channel();
    let cli = std::thread::spawn(move || {
        let args = [
            "davy",
            "project",
            "retention",
            "api",
            "--data",
            &d,
            "--logs",
            "9",
        ];
        let Command::Project { command } = Cli::try_parse_from(args).unwrap().command else {
            panic!("not the project command");
        };
        run_project(&mut vec![], command).unwrap();
        done.send(()).unwrap();
    });
    let logs_days = || {
        let c = store::open(&data).unwrap();
        retention::stored(&c, "api")
            .unwrap()
            .unwrap()
            .logs_days
            .map(retention::Days::get)
    };
    let wait = std::time::Duration::from_millis(300);
    assert!(changed.recv_timeout(wait).is_err());
    assert_eq!(logs_days(), None);
    drop(held);
    changed
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    assert_eq!(logs_days(), Some(9));
    cli.join().unwrap();
    std::fs::remove_dir_all(&data).unwrap();
}

#[test]
fn project_retention_prints_and_changes_a_projects_own_settings() {
    let data = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join(format!("test-cli-retention-{}", uuid::Uuid::new_v4()));
    store::create_project(&store::open(&data).unwrap(), "api").unwrap();
    let d = data.to_str().unwrap().to_owned();
    let run = |extra: &[&str]| {
        let args = ["davy", "project", "retention", "api", "--data", &d];
        let parsed = Cli::try_parse_from(args.iter().chain(extra))?;
        let Command::Project { command } = parsed.command else {
            panic!("not the project command");
        };
        let mut out = vec![];
        run_project(&mut out, command)
            .map_err(|e| clap::Error::raw(clap::error::ErrorKind::InvalidValue, e))?;
        Ok::<_, clap::Error>(String::from_utf8(out).unwrap())
    };
    let shown = |logs: &str, errors: &str| {
        format!("{{\"errors_days\":{errors},\"logs_days\":{logs},\"project\":\"api\"}}\n")
    };
    assert_eq!(run(&[]).unwrap(), shown("null", "null"));
    assert_eq!(run(&["--errors", "90"]).unwrap(), shown("null", "90"));
    assert_eq!(run(&["--logs", "7"]).unwrap(), shown("7", "90"));
    assert_eq!(run(&["--errors", "default"]).unwrap(), shown("7", "null"));
    assert_eq!(run(&[]).unwrap(), shown("7", "null"));
    for bad in ["0", "36501", "forever"] {
        assert!(run(&["--logs", bad]).is_err(), "{bad}");
    }
    assert_eq!(run(&[]).unwrap(), shown("7", "null"));
    let args = [
        "davy",
        "project",
        "retention",
        "web",
        "--data",
        &d,
        "--logs",
        "3",
    ];
    let Command::Project { command } = Cli::try_parse_from(args).unwrap().command else {
        panic!("not the project command");
    };
    let refused = run_project(&mut vec![], command).unwrap_err();
    assert_eq!(refused.to_string(), "unknown project \"web\"");
    std::fs::remove_dir_all(&data).unwrap();
}
