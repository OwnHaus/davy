use super::*;

#[test]
fn a_store_that_a_newer_davy_migrated_is_refused_unchanged() {
    let root = crate::test_support::scratch("store-newer");
    let newer = i64::try_from(MIGRATIONS.len()).unwrap() + 1;
    let c = open(&root).unwrap();
    c.pragma_update(None, "user_version", newer).unwrap();
    drop(c);
    let refused = open(&root).unwrap_err().to_string();
    assert!(refused.contains("newer Davy"), "{refused}");
    assert!(refused.contains("restore the backup"), "{refused}");
    let c = Connection::open(root.join(DB_FILE)).unwrap();
    let version: i64 = c
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, newer);
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn an_upgrade_builds_the_issue_timestamp_index_in_its_migration() {
    let root = crate::test_support::scratch("store-issue-timestamp");
    let c = open(&root).unwrap();
    c.execute_batch("DROP INDEX events_issue_timestamp; PRAGMA user_version=15;")
        .unwrap();
    c.execute_batch(crate::usage::UNDO).unwrap();
    drop(c);
    let c = open(&root).unwrap();
    let built: i64 = c
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name='events_issue_timestamp'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let version: i64 = c
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    let latest = i64::try_from(MIGRATIONS.len()).unwrap();
    assert_eq!((built, version), (1, latest));
    std::fs::remove_dir_all(&root).unwrap();
}
