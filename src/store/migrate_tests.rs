use crate::store;
use rusqlite::Connection;
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// A data directory whose database lacks migrations 14 and 15, like one an
/// older Davy wrote.
fn behind() -> PathBuf {
    let data = crate::test_support::scratch("migrate");
    store::open(&data)
        .unwrap()
        .execute_batch(&format!(
            "{} ALTER TABLE projects DROP COLUMN scrub; PRAGMA user_version=13;",
            crate::retention::UNDO
        ))
        .unwrap();
    data
}

/// A connection that has read the database, like an older server that still
/// runs.
fn holder(data: &Path) -> Connection {
    let c = Connection::open(data.join(store::DB_FILE)).unwrap();
    let _: i64 = c
        .query_row("SELECT count(*) FROM projects", [], |r| r.get(0))
        .unwrap();
    c
}

fn version(c: &Connection) -> i64 {
    c.query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap()
}

/// Whether migration 14's `id_floor` table and migration 15's `scrub` column
/// exist.
fn migrated_parts(c: &Connection) -> (i64, i64) {
    c.query_row(
        "SELECT (SELECT count(*) FROM sqlite_schema WHERE name='id_floor'), (SELECT count(*) FROM pragma_table_info('projects') WHERE name='scrub')",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .unwrap()
}

#[test]
fn a_pending_migration_is_refused_while_another_connection_has_the_database_open() {
    let data = behind();
    let old = holder(&data);
    let err = store::open(&data).unwrap_err().to_string();
    assert!(
        err.ends_with("is open in another process, and this Davy must migrate it with no other process using it. Stop the running Davy server and any other davy command, then run this again"),
        "{err}"
    );
    assert_eq!(version(&old), 13);
}

#[test]
fn a_refused_migration_leaves_no_partial_schema() {
    let data = behind();
    let old = holder(&data);
    store::open(&data).unwrap_err();
    assert_eq!(migrated_parts(&old), (0, 0));
}

#[test]
fn a_refused_migration_does_not_wait_the_busy_timeout() {
    let data = behind();
    let _old = holder(&data);
    let started = Instant::now();
    store::open(&data).unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn a_pending_migration_runs_once_the_other_connection_closes() {
    let data = behind();
    let old = holder(&data);
    store::open(&data).unwrap_err();
    drop(old);
    let c = store::open(&data).unwrap();
    assert_eq!((version(&c), migrated_parts(&c)), (15, (1, 1)));
}

#[test]
fn the_store_after_a_migration_is_shared_in_wal_mode_with_foreign_keys() {
    let data = behind();
    let c = store::open(&data).unwrap();
    let other = store::open(&data).unwrap();
    store::create_project(&other, "api").unwrap();
    let state: (String, String, i64, i64) = c
        .query_row(
            "SELECT (SELECT journal_mode FROM pragma_journal_mode), (SELECT locking_mode FROM pragma_locking_mode), (SELECT foreign_keys FROM pragma_foreign_keys), (SELECT count(*) FROM projects)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(state, ("wal".into(), "normal".into(), 1, 1));
}

#[test]
fn an_open_without_pending_migrations_shares_the_database_as_before() {
    let data = crate::test_support::scratch("migrate");
    drop(store::open(&data).unwrap());
    let old = holder(&data);
    let first = store::open(&data).unwrap();
    let second = store::open(&data).unwrap();
    store::create_project(&first, "api").unwrap();
    store::create_project(&second, "web").unwrap();
    let slugs: String = old
        .query_row("SELECT group_concat(slug, ',') FROM projects", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(slugs, "api,web");
}
