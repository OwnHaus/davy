use super::super::{MIGRATIONS, SCHEMA};
use crate::store;
use rusqlite::Connection;
use std::{
    path::{Path, PathBuf},
    sync::Barrier,
    thread,
    time::{Duration, Instant},
};

const REFUSED: &str = "is open in another process, and this Davy must migrate it while no other process has it open. If that process is an older Davy server, stop it or restart it on this version. Otherwise close the program that holds the file, then run this again";

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

/// Opens the store in `data` with one attempt to migrate it, as `store::open`
/// does once its wait is over.
fn open_once(data: &Path) -> anyhow::Result<Connection> {
    super::open(&data.join(store::DB_FILE), Duration::ZERO)
}

/// Opens the store in `data` from eight threads at once and returns the
/// version each one saw, or its error.
fn open_together(data: &Path) -> Vec<Result<usize, String>> {
    let start = Barrier::new(8);
    thread::scope(|s| {
        let threads: Vec<_> = (0..8)
            .map(|_| {
                s.spawn(|| {
                    start.wait();
                    store::open(data)
                        .map(|c| version(&c))
                        .map_err(|e| e.to_string())
                })
            })
            .collect();
        threads.into_iter().map(|t| t.join().unwrap()).collect()
    })
}

/// The schema of a store that ran every migration from the start.
fn current_schema() -> String {
    schema(&store::open(&crate::test_support::scratch("migrate")).unwrap())
}

#[test]
fn a_pending_migration_is_refused_while_another_connection_has_the_database_open() {
    let data = behind();
    let old = holder(&data);
    let err = open_once(&data).unwrap_err().to_string();
    assert!(err.ends_with(REFUSED), "{err}");
    assert_eq!(version(&old), BEHIND);
}

#[test]
fn a_read_only_connection_stops_a_migration_too() {
    let data = behind();
    let old = Connection::open_with_flags(
        data.join(store::DB_FILE),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    assert_eq!(version(&old), BEHIND);
    let err = open_once(&data).unwrap_err().to_string();
    assert!(err.ends_with(REFUSED), "{err}");
}

#[test]
fn a_refused_migration_leaves_no_partial_schema() {
    let data = behind();
    let old = holder(&data);
    let before = schema(&old);
    open_once(&data).unwrap_err();
    assert_eq!((version(&old), schema(&old)), (BEHIND, before));
}

#[test]
fn one_attempt_to_migrate_does_not_wait_the_busy_timeout() {
    let data = behind();
    let _old = holder(&data);
    let started = Instant::now();
    open_once(&data).unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn the_store_keeps_trying_to_migrate_for_ten_seconds_before_it_refuses() {
    let data = behind();
    let _old = holder(&data);
    let started = Instant::now();
    let err = store::open(&data).unwrap_err().to_string();
    assert!(err.ends_with(REFUSED), "{err}");
    assert!(started.elapsed() >= Duration::from_secs(10));
}

#[test]
fn a_pending_migration_runs_once_the_other_connection_closes() {
    let data = behind();
    let old = holder(&data);
    open_once(&data).unwrap_err();
    drop(old);
    let c = store::open(&data).unwrap();
    assert_eq!(
        (version(&c), schema(&c)),
        (MIGRATIONS.len(), current_schema())
    );
}

#[test]
fn processes_that_open_an_outdated_store_together_all_get_it_migrated() {
    for _ in 0..5 {
        let opened = open_together(&behind());
        assert_eq!(opened, vec![Ok(MIGRATIONS.len()); 8]);
    }
}

#[test]
fn processes_that_open_a_fresh_directory_together_all_get_the_store() {
    for _ in 0..5 {
        let opened = open_together(&crate::test_support::scratch("migrate"));
        assert_eq!(opened, vec![Ok(MIGRATIONS.len()); 8]);
    }
}

#[test]
fn a_store_another_davy_is_migrating_names_that_as_the_reason() {
    let data = behind();
    let migrating = Connection::open(data.join(store::DB_FILE)).unwrap();
    migrating
        .pragma_update(None, "locking_mode", "EXCLUSIVE")
        .unwrap();
    migrating.execute_batch("BEGIN EXCLUSIVE; COMMIT;").unwrap();
    let err = open_once(&data).unwrap_err().to_string();
    assert!(
        err.ends_with("is locked by another process, most likely a davy that migrates it. Run this again when it is done"),
        "{err}"
    );
}

#[test]
fn a_connection_that_has_not_read_yet_does_not_stop_a_migration() {
    let data = behind();
    let idle = Connection::open(data.join(store::DB_FILE)).unwrap();
    drop(open_once(&data).unwrap());
    assert_eq!(version(&idle), MIGRATIONS.len());
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
