use super::super::{MIGRATIONS, SCHEMA};
use crate::store;
use rusqlite::Connection;
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// The version of a database that lacks only the newest migration.
const BEHIND: usize = MIGRATIONS.len() - 1;

/// A data directory whose database lacks the newest migration, like one the
/// previous Davy wrote. It is built from the first schema and the migrations
/// before the newest, so it stays one behind when later releases add some.
fn behind() -> PathBuf {
    let data = crate::test_support::scratch("migrate");
    std::fs::create_dir_all(&data).unwrap();
    let c = Connection::open(data.join(store::DB_FILE)).unwrap();
    c.execute_batch(SCHEMA).unwrap();
    for sql in &MIGRATIONS[..BEHIND] {
        c.execute_batch(sql).unwrap();
    }
    c.execute_batch(&format!("PRAGMA user_version={BEHIND}"))
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

fn version(c: &Connection) -> usize {
    let done: u32 = c
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    usize::try_from(done).unwrap()
}

/// Every table, index and trigger with its SQL, in name order.
fn schema(c: &Connection) -> String {
    c.query_row(
        "SELECT group_concat(name || ' ' || coalesce(sql, ''), char(10)) FROM (SELECT name, sql FROM sqlite_schema ORDER BY name)",
        [],
        |r| r.get(0),
    )
    .unwrap()
}

/// The schema of a store that ran every migration from the start.
fn current_schema() -> String {
    schema(&store::open(&crate::test_support::scratch("migrate")).unwrap())
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
    assert_eq!(version(&old), BEHIND);
}

#[test]
fn a_refused_migration_leaves_no_partial_schema() {
    let data = behind();
    let old = holder(&data);
    let before = schema(&old);
    store::open(&data).unwrap_err();
    assert_eq!((version(&old), schema(&old)), (BEHIND, before));
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
    assert_eq!(
        (version(&c), schema(&c)),
        (MIGRATIONS.len(), current_schema())
    );
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
