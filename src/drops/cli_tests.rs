use super::*;
use crate::drops::{Key, add, write};
use jiff::civil::date;
use std::path::Path;

/// 2026-11-25T10:00:00Z in milliseconds.
const NOON_MS: i64 = 1_795_600_800_000;

fn shop() -> Connection {
    let mut c = Connection::open_in_memory().unwrap();
    store::init(&mut c).unwrap();
    store::create_project(&c, "shop").unwrap();
    c
}

fn listed(c: &Connection) -> Vec<Listed> {
    list(c, None, Path::new("/srv/my data"), NOON_MS).unwrap()
}

/// A live rule of a person that dropped 4 lines and an agent's shadow rule
/// with quotes and a backslash in its text that would drop 9.
fn two_rules() -> Connection {
    let c = shop();
    let live = Spec::new(None, Some("debug"), None, None, None).unwrap();
    add(&c, 1, &live, true, By::Person, NOON_MS)
        .unwrap()
        .unwrap();
    let shadow = Spec::new(None, None, Some(r#"poll "a\b""#), Some("api"), Some("10%")).unwrap();
    add(&c, 1, &shadow, false, By::Agent, NOON_MS)
        .unwrap()
        .unwrap();
    let day = date(2026, 11, 25);
    let key = |rule, live| Key {
        day,
        rule,
        live,
        project: 1,
    };
    let n = |records, bytes| Count { records, bytes };
    write(
        &c,
        &[(key(1, true), n(4, 400)), (key(2, false), n(9, 1_800))],
    )
    .unwrap();
    c
}

#[test]
fn a_list_line_says_what_a_rule_counted_and_how_a_person_makes_it_live() {
    let lines: Vec<String> = listed(&two_rules()).iter().map(Listed::line).collect();
    assert_eq!(
        lines,
        [
            "1     shop                 live   person level debug and below, drop all: dropped 4 lines (400 B) in 7 days",
            r#"2     shop                 shadow agent  message contains "poll \"a\\b\"" service "api", keep 10%: would drop 9 lines (1.8 KB) in 7 days, expires 2026-12-09T10:00:00Z; a person can run: davy drop live --data '/srv/my data' 2"#,
        ]
    );
}

#[test]
fn listed_json_gives_raw_bytes_and_the_command_for_a_shadow_rule() {
    let rules = listed(&two_rules());
    let shadow = rules[1].json(false);
    assert_eq!(
        shadow,
        json!({
            "id": 2,
            "project": "shop",
            "state": "shadow",
            "by": "agent",
            "position": 2,
            "pattern": null,
            "any_level": false,
            "level": null,
            "message": r#"poll "a\b""#,
            "service": "api",
            "keep_percent": 10,
            "created_at": "2026-11-25T10:00:00Z",
            "expires_at": "2026-12-09T10:00:00Z",
            "last_7_days": {
                "dropped": {"records": 0, "bytes": 0},
                "would_drop": {"records": 9, "bytes": 1_800},
            },
            "make_live": "a person can run: davy drop live --data '/srv/my data' 2",
        })
    );
    assert_eq!(
        rules[0].json(true)["last_7_days"]["dropped"]["bytes"],
        "400 B"
    );
    assert_eq!(rules[0].json(true).get("make_live"), None);
}

#[test]
fn describe_words_each_way_a_rule_matches() {
    let spec = |p, l, m, svc| Spec::new(p, l, m, svc, None).unwrap();
    let mut any = spec(Some("00000000abcd"), None, None, None);
    assert_eq!(describe(&any), "pattern 00000000abcd below warn, drop all");
    any.any_level = true;
    assert_eq!(
        describe(&any),
        "pattern 00000000abcd at any level, drop all"
    );
    assert_eq!(
        describe(&spec(None, None, Some("ok*"), Some(r"a\b"))),
        r#"message like "ok*" service "a\\b", drop all"#
    );
    assert_eq!(
        describe(&spec(None, None, Some(r#"say "ok""#), None)),
        r#"message contains "say \"ok\"", drop all"#
    );
}

#[test]
fn a_list_gives_the_absolute_data_directory_in_its_command() {
    let rules = list(&two_rules(), None, Path::new("my data"), NOON_MS).unwrap();
    let here = std::env::current_dir().unwrap().join("my data");
    assert_eq!(
        rules[1].live_command(),
        format!(
            "davy drop live --data {} 2",
            sh(&here.display().to_string())
        )
    );
}

/// A data directory with the project shop and a shadow rule 1 of a
/// person that would have dropped 9 lines.
fn on_disk(name: &str) -> PathBuf {
    let data = crate::test_support::scratch(name);
    let c = store::open(&data).unwrap();
    store::create_project(&c, "shop").unwrap();
    let spec = Spec::new(None, Some("debug"), None, None, None).unwrap();
    add(&c, 1, &spec, false, By::Person, time::now_ms())
        .unwrap()
        .unwrap();
    let key = Key {
        day: time::day_of(time::now_ms() * 1_000_000),
        rule: 1,
        live: false,
        project: 1,
    };
    write(
        &c,
        &[(
            key,
            Count {
                records: 9,
                bytes: 900,
            },
        )],
    )
    .unwrap();
    data
}

fn live(data: &Path, yes: bool, answer: Option<&str>) -> (Result<()>, String, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let mut answer = answer.map(std::io::Cursor::new);
    let done = run(
        &mut out,
        &mut err,
        answer.as_mut().map(|a| a as &mut dyn BufRead),
        Drop::Live {
            rule_id: 1,
            yes,
            data: data.to_owned(),
        },
    );
    let text = |b: Vec<u8>| String::from_utf8(b).unwrap();
    (done, text(out), text(err))
}

#[test]
fn live_shows_the_rule_and_asks_unless_told_yes() {
    let data = on_disk("drop-live");
    let shown = "rule 1 of shop: level debug and below, drop all\nin the last 7 days it would have dropped 9 lines (900 B)\n";
    let (done, out, err) = live(&data, false, None);
    assert_eq!(
        done.unwrap_err().to_string(),
        "davy drop live asks before a rule drops lines, and there is no terminal to ask on: add --yes to make rule 1 live"
    );
    assert_eq!((out.as_str(), err.as_str()), ("", shown));
    let (done, out, err) = live(&data, false, Some("\n"));
    assert_eq!(done.unwrap_err().to_string(), "rule 1 stays a shadow rule");
    assert_eq!(out, "");
    assert_eq!(
        err,
        format!("{shown}make it live, so these lines are dropped from now on? [y/N] ")
    );
    let (done, out, _) = live(&data, false, Some("y\n"));
    done.unwrap();
    let rule: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(rule["state"], "live");
    std::fs::remove_dir_all(&data).unwrap();
}

#[test]
fn live_with_yes_asks_nothing() {
    let data = on_disk("drop-live-yes");
    let (done, out, err) = live(&data, true, None);
    done.unwrap();
    assert_eq!(err, "");
    let rule: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(rule["state"], "live");
    std::fs::remove_dir_all(&data).unwrap();
}

#[test]
fn the_commands_that_change_a_rule_neither_create_nor_upgrade_a_store() {
    let data = crate::test_support::scratch("drop-missing");
    let commands = || {
        [
            Drop::List {
                data: data.clone(),
                json: false,
                project: None,
            },
            Drop::Live {
                rule_id: 1,
                yes: true,
                data: data.clone(),
            },
            Drop::Shadow {
                rule_id: 1,
                data: data.clone(),
            },
            Drop::Remove {
                rule_id: 1,
                data: data.clone(),
            },
        ]
    };
    let missing = format!(
        "no database in {}: run davy serve --data {} first",
        data.display(),
        data.display()
    );
    for command in commands() {
        let e = run(&mut Vec::new(), &mut Vec::new(), None, command).unwrap_err();
        assert_eq!(e.to_string(), missing);
    }
    assert!(!data.exists());
    let c = store::open(&data).unwrap();
    c.pragma_update(None, "user_version", 1).unwrap();
    drop(c);
    let older = format!(
        "the database in {} is from an older Davy: run davy serve --data {} once to upgrade it",
        data.display(),
        data.display()
    );
    for command in commands() {
        let e = run(&mut Vec::new(), &mut Vec::new(), None, command).unwrap_err();
        assert_eq!(e.to_string(), older);
    }
    std::fs::remove_dir_all(&data).unwrap();
}

#[test]
fn a_live_add_and_a_remove_leave_a_row_naming_the_rule_and_no_change_leaves_none() {
    let data = on_disk("drop-ledger");
    let add = |shadow, message: &str| Drop::Add {
        pattern: None,
        level: None,
        message: Some(message.into()),
        service: None,
        keep: None,
        shadow,
        any_level: false,
        data: data.clone(),
        project: "shop".into(),
    };
    let remove = |rule_id| Drop::Remove {
        rule_id,
        data: data.clone(),
    };
    // Adding the live rule again changes nothing and leaves no row.
    let ran: Vec<bool> = [
        add(true, "tick"),
        add(false, "poll"),
        add(false, "poll"),
        remove(1),
        remove(9),
    ]
    .into_iter()
    .map(|command| run(&mut Vec::new(), &mut Vec::new(), None, command).is_ok())
    .collect();
    assert_eq!(ran, [true, true, true, true, false]);
    let c = store::open_current(&data).unwrap();
    let rows: Vec<_> = crate::ledger::list(&c, None, None, 50)
        .unwrap()
        .into_iter()
        .map(|r| (r.actor, r.action, r.project, r.target, r.outcome, r.summary))
        .collect();
    let error = crate::ledger::Outcome::Error;
    let done = crate::ledger::Outcome::Done;
    let rule = |id| Some(format!("rule {id}"));
    let cli = || crate::ledger::CLI.to_owned();
    assert_eq!(
        rows,
        [
            (
                cli(),
                "drop_rule_remove".into(),
                None,
                rule(9),
                error,
                "removed rule 9; no rule 9".into()
            ),
            (
                cli(),
                "drop_rule_remove".into(),
                Some("shop".into()),
                rule(1),
                done,
                "removed shadow rule 1 for level debug and below, drop all".into()
            ),
            (
                cli(),
                "drop_rule_add".into(),
                Some("shop".into()),
                rule(3),
                done,
                r#"added live rule 3 for message contains "poll", drop all"#.into()
            ),
        ]
    );
    drop(c);
    std::fs::remove_dir_all(&data).unwrap();
}
