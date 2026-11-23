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
