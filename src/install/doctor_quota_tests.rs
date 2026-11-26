use super::*;
use crate::quota::Settings;

const D: &str = "/var/lib/davy";

fn project(
    slug: &str,
    errors: Option<u64>,
    logs: Option<u64>,
    today: [u64; 2],
) -> (String, status::Status) {
    let settings = Settings {
        errors,
        logs,
        ..Settings::default()
    };
    let s = status::Status {
        settings,
        today,
        ..status::Status::default()
    };
    (slug.to_owned(), s)
}

#[test]
fn no_quota_is_ok() {
    assert_eq!(check(&[], D), ok("quotas", "no project has a quota"));
}

#[test]
fn quotas_not_reached_are_ok_even_near_the_line() {
    let one = [project("shop", Some(100), None, [99, 0])];
    assert_eq!(
        check(&one, D),
        ok("quotas", "1 project has a quota, not reached today")
    );
    let two = [one[0].clone(), project("blog", None, Some(10), [5, 9])];
    assert_eq!(
        check(&two, D),
        ok("quotas", "2 projects have a quota, none reached today")
    );
}

#[test]
fn a_quota_reached_warns_with_todays_use_and_the_command_that_raises_it() {
    let projects = [
        project("blog", Some(10), Some(2_000_000_000), [3, 2_100_000_000]),
        project("shop", Some(100), None, [100, 0]),
    ];
    assert_eq!(
        check(&projects, D),
        not_ok(
            "quotas",
            Status::Warn,
            "blog used 2.1 GB of its 2.0 GB logs quota today (105%); shop used 100 of its 100 errors quota today (100%); ingest refuses it until midnight UTC",
            "davy project quota blog --data /var/lib/davy --logs SIZE",
        )
    );
}

#[test]
fn the_command_quotes_a_slug_a_shell_would_split() {
    let projects = [project("my shop", Some(1), None, [1, 0])];
    let Check { fix, .. } = check(&projects, "'/srv/my data'");
    assert_eq!(
        fix.as_deref(),
        Some("davy project quota 'my shop' --data '/srv/my data' --errors N")
    );
}

#[test]
fn the_check_reads_the_stored_quotas_and_todays_use() {
    let dir = crate::test_support::scratch("doctor-quota");
    std::fs::create_dir_all(&dir).unwrap();
    let mut c = crate::store::open(&dir).unwrap();
    crate::store::init(&mut c).unwrap();
    crate::store::create_project(&c, "shop").unwrap();
    crate::store::create_project(&c, "blog").unwrap();
    quota::settings::store(
        &c,
        1,
        &Settings {
            errors: Some(2),
            ..Settings::default()
        },
    )
    .unwrap();
    let now = crate::time::now_ns();
    let row = usage::Row {
        day: crate::time::day_of(now),
        project: 1,
        signal: usage::Signal::Errors,
        count: usage::Count::new(2, 10),
    };
    usage::write(&c, &[row]).unwrap();
    drop(c);
    let got = quotas(&dir, D, now);
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(got.status, Status::Warn);
    assert_eq!(
        got.detail,
        "shop used 2 of its 2 errors quota today (100%); ingest refuses it until midnight UTC"
    );
    let empty = crate::test_support::scratch("doctor-quota");
    assert_eq!(
        quotas(&empty, D, now),
        ok("quotas", "no project has a quota")
    );
}
