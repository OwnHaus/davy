use super::*;
use crate::{self_report::status::Crash, test_support::scratch};

const D: &str = "/var/lib/davy";

fn crashed(issue_id: i64, title: &str) -> Crash {
    Crash {
        issue_id,
        title: title.to_owned(),
        last_seen: "2026-01-01T00:00:00.000Z".to_owned(),
        event_count: 2,
        recorded_at: "2026-01-02T00:00:00.000Z".to_owned(),
    }
}

#[test]
fn nothing_reported_is_ok() {
    assert_eq!(
        check(&Health::waiting(0), D),
        ok(
            "davy",
            "no crash, error or warning of Davy's own in the last 24 hours"
        )
    );
}

#[test]
fn lines_without_a_crash_are_ok_and_name_the_command_that_lists_them() {
    let h = Health {
        warnings_24h: 3,
        errors_24h: 1,
        ..Health::waiting(0)
    };
    assert_eq!(
        check(&h, D),
        ok(
            "davy",
            "no crash in the last 24 hours; 1 errors and 3 warnings of Davy's own in the last 24 hours, listed by davy logs --data /var/lib/davy --project davy --level warn"
        )
    );
}

#[test]
fn a_crash_warns_and_names_the_command_and_the_mcp_call() {
    let h = Health {
        warnings_24h: 0,
        errors_24h: 2,
        crashes_24h: vec![
            crashed(7, "panic: index out of bounds"),
            crashed(3, "panic: x"),
        ],
        crashes_waiting: 1,
    };
    assert_eq!(
        check(&h, D),
        not_ok(
            "davy",
            Status::Warn,
            r#"Davy crashed in the last 24 hours: issue 7 "panic: index out of bounds", 2 events, the last at 2026-01-01T00:00:00.000Z, recorded at 2026-01-02T00:00:00.000Z and 1 other crash issues; 2 errors and 0 warnings of Davy's own in the last 24 hours; 1 crash files wait in /var/lib/davy/crashes"#,
            "davy context 7 --data /var/lib/davy, or the MCP call issue_context with issue_id 7"
        )
    );
}

#[test]
fn a_waiting_crash_file_warns_and_says_what_records_it() {
    assert_eq!(
        check(&Health::waiting(2), D),
        not_ok(
            "davy",
            Status::Warn,
            "Davy crashed: 2 crash files wait in /var/lib/davy/crashes to be recorded; 0 errors and 0 warnings of Davy's own in the last 24 hours",
            "davy serve --data /var/lib/davy records them in the project davy within 5 seconds, unless it runs with --no-self-report"
        )
    );
}

#[test]
fn a_data_directory_without_a_database_counts_crash_files_only() {
    let data = scratch("doctor-davy");
    let d = data.display().to_string();
    assert_eq!(
        davy(&data, &d, 0),
        ok(
            "davy",
            "no crash, error or warning of Davy's own in the last 24 hours"
        )
    );
    let dir = crash::dir(&data);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("1767225600000-0123456789abcdef0123456789abcdef.json"),
        "{}",
    )
    .unwrap();
    assert_eq!(davy(&data, &d, 0).status, Status::Warn);
    std::fs::remove_dir_all(&data).unwrap();
}
