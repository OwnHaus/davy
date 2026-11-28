use super::*;
use crate::test_support::scratch;

const D: &str = "/var/lib/davy";
const NOW: i64 = 1_800_000_000_000;

fn record(at_ms: i64) -> Record {
    Record {
        at_ms,
        davy_version: "0.3.0".to_owned(),
        schema: 30,
        files: 12,
        bytes: 2_000_000,
        written: 500_000,
        to: "/var/backups/davy/a.tar.zst".to_owned(),
    }
}

fn fix() -> String {
    format!(
        "sudo runuser -u davy -- davy backup --data {D} /var/backups/davy/davy-$(date +%F).tar.zst, and a timer that runs it daily: see Backups in docs/install.md"
    )
}

#[test]
fn a_recent_backup_is_ok_and_says_what_it_took() {
    let data = scratch("doctor-backup");
    std::fs::create_dir_all(&data).unwrap();
    record(NOW - 2 * DAY_MS).save(&data).unwrap();
    assert_eq!(
        backup(&data, D, NOW),
        ok(
            NAME,
            "the last backup is 2 days old: 2.0 MB of files as 500 KB at /var/backups/davy/a.tar.zst"
        )
    );
}

#[test]
fn a_backup_older_than_a_week_warns() {
    let data = scratch("doctor-backup");
    std::fs::create_dir_all(&data).unwrap();
    record(NOW - 8 * DAY_MS).save(&data).unwrap();
    assert_eq!(
        backup(&data, D, NOW),
        not_ok(
            NAME,
            Status::Warn,
            "the last backup is 8 days old: 2.0 MB of files as 500 KB at /var/backups/davy/a.tar.zst",
            fix()
        )
    );
}

#[test]
fn no_backup_of_a_new_database_is_ok_and_of_an_old_one_warns() {
    let data = scratch("doctor-backup");
    drop(crate::store::open(&data).unwrap());
    let now = crate::time::now_ms();
    let check = backup(&data, D, now);
    if age_ms(&data.join(DB_FILE), now).is_some() {
        assert_eq!(
            check,
            ok(
                NAME,
                "no backup taken yet, and the database is less than a day"
            )
        );
    }
    assert_eq!(
        backup(&data, D, now + 8 * DAY_MS),
        not_ok(
            NAME,
            Status::Warn,
            format!("davy backup never took {D}"),
            fix()
        )
    );
}

#[test]
fn a_record_that_is_not_read_warns() {
    let data = scratch("doctor-backup");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join(crate::backup::RECORD), "{").unwrap();
    let check = backup(&data, D, NOW);
    assert_eq!(check.status, Status::Warn);
    assert!(
        check.detail.starts_with("reading last-backup.json: "),
        "{}",
        check.detail
    );
}
