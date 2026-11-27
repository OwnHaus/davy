use super::*;
use crate::{event::ErrorEvent, store};
use serde_json::json;
use std::cell::RefCell;

#[test]
fn a_lock_on_the_data_directory_waits_for_another_process_holding_it() {
    let data = crate::test_support::scratch("retention-lock-dir");
    std::fs::create_dir_all(&data).unwrap();
    // Two locks on one data directory, as the server and the CLI have.
    let (server, cli) = (
        Lock::default().with_dir(&data),
        Lock::default().with_dir(&data).waiting(CLI_WAIT),
    );
    let held = server.hold_all().unwrap();
    let (took, taken) = std::sync::mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let _held = cli.hold_all().unwrap();
        took.send(()).unwrap();
    });
    let wait = Duration::from_millis(300);
    assert_eq!(
        taken.recv_timeout(wait),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
    );
    drop(held);
    assert_eq!(taken.recv_timeout(Duration::from_secs(10)), Ok(()));
    waiter.join().unwrap();
    std::fs::remove_dir_all(&data).unwrap();
}

#[test]
fn a_wait_for_the_data_directory_ends_with_an_error_naming_the_holder() {
    let data = crate::test_support::scratch("retention-lock-wait");
    std::fs::create_dir_all(&data).unwrap();
    // A CLI that stopped while it held the lock.
    let cli = Lock::default().with_dir(&data).waiting(CLI_WAIT);
    let held = cli.hold_all().unwrap();
    let server = Lock::default()
        .with_dir(&data)
        .waiting(Duration::from_millis(50));
    let (gave_up, given_up) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let tried = server.hold_all().map(drop).map_err(|e| e.to_string());
        gave_up.send(tried).unwrap();
    });
    let tried = given_up.recv_timeout(Duration::from_secs(10)).unwrap();
    let expected = format!(
        "{} is still locked after 0.05 s: another Davy process holds it, such as a server deleting logs or a stopped `davy project retention`",
        data.display()
    );
    assert_eq!(tried, Err(expected));
    drop(held);
    std::fs::remove_dir_all(&data).unwrap();
}

#[test]
fn the_lock_creates_no_file_and_a_file_another_user_left_cannot_stop_it() {
    use std::os::unix::fs::PermissionsExt as _;
    let data = crate::test_support::scratch("retention-lock-left");
    std::fs::create_dir_all(&data).unwrap();
    // What a CLI run as root left under a umask of 077, as this process
    // sees it: a file it may not open.
    let left = data.join("retention.lock");
    std::fs::write(&left, "").unwrap();
    std::fs::set_permissions(&left, std::fs::Permissions::from_mode(0o000)).unwrap();
    let lock = Lock::default().with_dir(&data);
    let held = lock.hold_all().map(drop);
    assert!(held.is_ok(), "{held:?}");
    let names: Vec<_> = std::fs::read_dir(&data)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, ["retention.lock"]);
    std::fs::remove_dir_all(&data).unwrap();
}

fn memory() -> Connection {
    let mut c = Connection::open_in_memory().unwrap();
    store::init(&mut c).unwrap();
    c
}

fn change(project: &str, logs: Option<Setting>, errors: Option<Setting>) -> Change {
    Change {
        project: project.to_owned(),
        logs_days: logs,
        errors_days: errors,
    }
}

#[test]
fn a_retention_is_1_to_36500_days() {
    assert_eq!(Days::new(1).map(Days::get), Ok(1));
    assert_eq!(Days::new(36_500).map(Days::get), Ok(36_500));
    for bad in [0, 36_501, u32::MAX] {
        assert_eq!(
            Days::new(bad),
            Err(format!("retention is 1 to 36500 days, not {bad}"))
        );
    }
    assert_eq!(Days::new(2).unwrap().cutoff_ms(3 * DAY_MS), DAY_MS);
}

#[test]
fn a_setting_is_a_number_of_days_or_the_word_default() {
    let default = Setting::Default(ServerDefault::Default);
    assert_eq!(Setting::parse("default"), Ok(default));
    assert_eq!(Setting::parse("7"), Ok(Setting::Days(7)));
    assert!(Setting::parse("0").is_err());
    assert!(Setting::parse("36501").is_err());
    assert!(Setting::parse("Default").is_err());
    assert!(Setting::parse("-1").is_err());
    let read = |v: serde_json::Value| serde_json::from_value::<Change>(v);
    let ok = read(json!({"project": "api", "logs_days": 7, "errors_days": "default"})).unwrap();
    assert_eq!(ok, change("api", Some(Setting::Days(7)), Some(default)));
    assert_eq!(ok.check(), Ok(()));
    assert!(read(json!({"project": "api", "logs_days": "forever"})).is_err());
    assert!(read(json!({"project": "api", "logs_days": -3})).is_err());
    assert!(read(json!({"project": "api", "days": 3})).is_err());
    let zero = read(json!({"project": "api", "errors_days": 0})).unwrap();
    assert_eq!(
        zero.check(),
        Err("retention is 1 to 36500 days, not 0".into())
    );
}

#[test]
fn a_change_sets_only_the_settings_it_names() {
    let c = memory();
    store::create_project(&c, "web").unwrap();
    store::create_project(&c, "api").unwrap();
    let days = |n| Some(Days::new(n).unwrap());
    let stored = |logs_days, errors_days| Stored {
        logs_days,
        errors_days,
    };
    let set = |ch: Change| set(&c, &ch, By::Person).unwrap();
    assert_eq!(stored_of(&c, "api"), stored(None, None));
    assert_eq!(
        set(change(
            "api",
            Some(Setting::Days(7)),
            Some(Setting::Days(400))
        )),
        Ok(stored(days(7), days(400)))
    );
    assert_eq!(
        set(change("api", None, Some(Setting::Days(90)))),
        Ok(stored(days(7), days(90)))
    );
    let default = Setting::Default(ServerDefault::Default);
    assert_eq!(
        set(change("api", Some(default), None)),
        Ok(stored(None, days(90)))
    );
    assert_eq!(
        set(change("api", Some(Setting::Days(0)), None)),
        Err("retention is 1 to 36500 days, not 0".into())
    );
    assert_eq!(stored_of(&c, "api"), stored(None, days(90)));
    assert_eq!(
        set(change("nope", Some(Setting::Days(3)), None)),
        Err(unknown("nope"))
    );
    assert_eq!(stored_of(&c, "web"), stored(None, None));
    assert_eq!(super::stored(&c, "nope").unwrap(), None);
}

#[test]
fn an_agent_may_only_keep_data_longer() {
    let c = memory();
    store::create_project(&c, "api").unwrap();
    let defaults = Defaults {
        logs: Days::new(30).unwrap(),
        errors: Days::new(365).unwrap(),
        hot_days: HotDays::default(),
    };
    let agent = |ch: Change| set(&c, &ch, By::Agent(defaults)).unwrap();
    let days = |n| Some(Days::new(n).unwrap());
    let default = Setting::Default(ServerDefault::Default);
    assert_eq!(
        agent(change("api", Some(Setting::Days(7)), None)),
        Err("an agent may only keep data longer: logs are kept 30 days and 7 would delete what is older for good. A person shortens it with `davy project retention` or in the web UI".into())
    );
    assert_eq!(
        agent(change("api", Some(Setting::Days(90)), Some(default))),
        Ok(Stored {
            logs_days: days(90),
            errors_days: None
        })
    );
    // Back to the default drops a setting of the project's own.
    assert!(agent(change("api", Some(default), None)).is_err());
    // One shorter setting refuses the whole change.
    assert!(
        agent(change(
            "api",
            Some(Setting::Days(100)),
            Some(Setting::Days(364))
        ))
        .is_err()
    );
    assert_eq!(
        stored_of(&c, "api"),
        Stored {
            logs_days: days(90),
            errors_days: None
        }
    );
    assert_eq!(
        agent(change("nope", Some(Setting::Days(3)), None)),
        Err(unknown("nope"))
    );
    // A person may shorten.
    assert_eq!(
        set(&c, &change("api", Some(default), None), By::Person).unwrap(),
        Ok(Stored::default())
    );
}

fn stored_of(c: &Connection, slug: &str) -> Stored {
    stored(c, slug).unwrap().unwrap()
}

#[test]
fn an_agent_cannot_return_a_projects_own_retention_to_the_default() {
    let c = memory();
    store::create_project(&c, "api").unwrap();
    store::create_project(&c, "web").unwrap();
    let defaults = Defaults {
        logs: Days::new(30).unwrap(),
        errors: Days::new(365).unwrap(),
        hot_days: HotDays::default(),
    };
    let agent = |ch: Change| set(&c, &ch, By::Agent(defaults)).unwrap();
    let default = Setting::Default(ServerDefault::Default);
    let own = change("api", Some(Setting::Days(7)), Some(Setting::Days(100)));
    set(&c, &own, By::Person).unwrap().unwrap();
    // The defaults keep both longer today, and could be lowered tomorrow.
    assert_eq!(
        agent(change("api", Some(default), None)),
        Err("an agent may not return logs to the server default: the project keeps them 7 days of its own, and a lower default would delete what it keeps. A person does it with `davy project retention` or in the web UI".into())
    );
    assert!(agent(change("api", None, Some(default))).is_err());
    let days = |n| Some(Days::new(n).unwrap());
    let kept = Stored {
        logs_days: days(7),
        errors_days: days(100),
    };
    assert_eq!(stored_of(&c, "api"), kept);
    // A project that follows the defaults may be told to follow them.
    assert_eq!(
        agent(change("web", Some(default), Some(default))),
        Ok(Stored::default())
    );
}
#[test]
fn what_a_project_keeps_falls_back_to_the_server_default() {
    let c = memory();
    store::create_project(&c, "web").unwrap();
    let (api, _) = store::create_project(&c, "api").unwrap();
    set(&c, &change("api", Some(Setting::Days(7)), None), By::Person)
        .unwrap()
        .unwrap();
    let defaults = Defaults {
        logs: Days::new(30).unwrap(),
        errors: Days::new(400).unwrap(),
        hot_days: HotDays::default(),
    };
    let now = 1_000 * DAY_MS;
    let all = all(&c, defaults, now).unwrap();
    let slugs: Vec<&str> = all.iter().map(|r| r.project.as_str()).collect();
    assert_eq!(slugs, ["api", "web"]);
    let api_kept = &all[0];
    assert_eq!(api_kept.logs.days.get(), 7);
    assert!(!api_kept.logs.server_default);
    assert_eq!(
        api_kept.logs.deletes_before,
        crate::time::rfc3339_ms(993 * DAY_MS)
    );
    assert_eq!(api_kept.errors.days.get(), 400);
    assert!(api_kept.errors.server_default);
    assert_eq!(all[1].logs.days.get(), 30);
    assert_eq!(
        one(&c, "api", defaults, now).unwrap().as_ref(),
        Some(api_kept)
    );
    assert_eq!(one(&c, "nope", defaults, now).unwrap(), None);
    let shown = serde_json::to_value(api_kept).unwrap();
    assert_eq!(shown["logs"]["days"], 7);
    assert_eq!(shown["errors"]["server_default"], true);
    let logs = log_days(&c).unwrap();
    assert_eq!(logs, HashMap::from([(api.cast_unsigned(), 7)]));
}

#[test]
fn the_schema_refuses_a_retention_out_of_range() {
    let c = memory();
    store::create_project(&c, "api").unwrap();
    for bad in [0, 36_501] {
        let r = c.execute("UPDATE projects SET error_retention_days=?1", [bad]);
        assert!(r.is_err(), "{bad}");
    }
}

#[test]
fn turns_give_each_class_a_batch_per_round_until_none_is_full() {
    let left = RefCell::new(vec![3_u32, 1, 0]);
    let calls = RefCell::new(Vec::new());
    turns(3, Duration::from_secs(60), |class| {
        calls.borrow_mut().push(class);
        let mut left = left.borrow_mut();
        left[class] = left[class].saturating_sub(1);
        Ok(left[class] > 0)
    })
    .unwrap();
    assert_eq!(*calls.borrow(), [0, 1, 2, 0, 0]);
}

#[test]
fn turns_run_one_round_even_with_no_budget_and_stop_at_the_deadline() {
    let mut calls = Vec::new();
    turns(2, Duration::ZERO, |class| {
        calls.push(class);
        Ok(true)
    })
    .unwrap();
    assert_eq!(calls, [0, 1]);
    let failed = turns(1, Duration::from_secs(60), |_| anyhow::bail!("disk"));
    assert_eq!(failed.unwrap_err().to_string(), "disk");
}

#[test]
fn the_migration_backfills_when_each_tag_was_last_seen() {
    let mut c = memory();
    store::create_project(&c, "old").unwrap();
    let at = |received_at: i64, env: &str, release: &str| {
        let mut e =
            ErrorEvent::parse(json!({"message": "boom", "environment": env, "release": release}))
                .unwrap();
        e.received_at = received_at;
        e
    };
    for e in [
        at(10, "prod", "v1"),
        at(30, "prod", "v2"),
        at(20, "dev", "v1"),
    ] {
        store::insert(&c, 1, &e).unwrap();
    }
    c.execute_batch(&format!(
        "{quota} {drops} {} {UNDO} ALTER TABLE projects DROP COLUMN scrub; DROP TABLE latest_payloads; DROP TABLE log_days; DROP TABLE store_days; ALTER TABLE projects DROP COLUMN vercel_secret; ALTER TABLE issues DROP COLUMN discarded_at; DROP TABLE discarded_daily; DROP TABLE searches; DROP TABLE search_days; DROP INDEX projects_own; ALTER TABLE projects DROP COLUMN own; PRAGMA user_version=13;",
        crate::usage::UNDO,
        drops = crate::drops::UNDO,
        quota = crate::quota::UNDO
    ))
    .unwrap();
    store::init(&mut c).unwrap();
    let tags = || -> Vec<(String, String, i64)> {
        let mut q = c
            .prepare("SELECT key, value, last_at FROM issue_tags ORDER BY key, value")
            .unwrap();
        q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    let row = |k: &str, v: &str, at| (k.to_owned(), v.to_owned(), at);
    assert_eq!(
        tags(),
        [
            row("environment", "dev", 20),
            row("environment", "prod", 30),
            row("release", "v1", 20),
            row("release", "v2", 30),
        ]
    );
    // The new trigger keeps the newest time, whatever order events come in.
    store::insert(&c, 1, &at(25, "dev", "v3")).unwrap();
    store::insert(&c, 1, &at(5, "prod", "v3")).unwrap();
    let after = tags();
    assert!(after.contains(&row("environment", "dev", 25)));
    assert!(after.contains(&row("environment", "prod", 30)));
    assert!(after.contains(&row("release", "v3", 25)));
}
