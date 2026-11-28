use crate::{event::ErrorEvent, store};
use rusqlite::Connection;
use serde_json::json;

/// The version this migration leaves and the one before. The fixtures
/// below take a store back to `BEFORE` by dropping `latest_payloads` and
/// what the migrations after it made, with `LATER`.
const AFTER: usize = 19;
const BEFORE: usize = AFTER - 1;
/// The version every migration leaves.
const LAST: usize = store::migrations::MIGRATIONS.len();
/// `crate::ledger::UNDO`, `crate::tokens::UNDO`, `crate::quota::UNDO`,
/// `crate::drops::UNDO`, `crate::usage::savings::UNDO`,
/// `crate::self_report::own::UNDO`, `crate::vercel::UNDO` then
/// `crate::usage::stored::UNDO`, newest first.
const LATER: &str = "DROP TABLE ledger; DROP TABLE agent_tokens; DELETE FROM sqlite_sequence WHERE name='agent_tokens'; DROP TABLE refused_requests; DROP TABLE quota_alerts; ALTER TABLE projects DROP COLUMN spike_logs; ALTER TABLE projects DROP COLUMN spike; ALTER TABLE projects DROP COLUMN quota_warn; ALTER TABLE projects DROP COLUMN quota_logs; ALTER TABLE projects DROP COLUMN quota_errors; DROP TABLE drop_daily; DROP TABLE drop_rules; DELETE FROM sqlite_sequence WHERE name='drop_rules'; ALTER TABLE issues DROP COLUMN discarded_at; DROP TABLE discarded_daily; DROP TABLE searches; DROP TABLE search_days; DROP INDEX projects_own; ALTER TABLE projects DROP COLUMN own; ALTER TABLE projects DROP COLUMN vercel_secret; DROP TABLE log_days; DROP TABLE store_days;";

#[test]
fn the_fixtures_undo_every_migration_after_the_newest_payload_one() {
    let later = &store::migrations::MIGRATIONS[AFTER..];
    assert_eq!(store::migrations::MIGRATIONS[BEFORE], super::MIGRATION);
    assert_eq!(
        later,
        [
            crate::usage::stored::MIGRATION,
            crate::vercel::MIGRATION,
            crate::self_report::own::MIGRATION,
            crate::usage::savings::MIGRATION,
            crate::drops::MIGRATION,
            crate::quota::MIGRATION,
            crate::tokens::MIGRATION,
            crate::ledger::MIGRATION
        ],
        "a later migration needs its undo in LATER"
    );
    assert_eq!(
        LATER,
        format!(
            "{} {} {} {} {} {} {} {}",
            crate::ledger::UNDO,
            crate::tokens::UNDO,
            crate::quota::UNDO,
            crate::drops::UNDO,
            crate::usage::savings::UNDO,
            crate::self_report::own::UNDO,
            crate::vercel::UNDO,
            crate::usage::stored::UNDO
        )
    );
}

fn version(n: usize) -> i64 {
    i64::try_from(n).unwrap()
}

fn memory() -> Connection {
    let mut c = Connection::open_in_memory().unwrap();
    store::init(&mut c).unwrap();
    store::create_project(&c, "shop").unwrap();
    c
}

/// Event `n` of the issue `message`, with 2 KB of text zstd cannot shrink.
fn event(message: &str, n: u64) -> ErrorEvent {
    let mut x = n.wrapping_add(1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let noise: String = (0..2_000)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            char::from(b'a' + u8::try_from(x % 26).unwrap())
        })
        .collect();
    ErrorEvent::parse(json!({"message":message,"extra":{"n":n,"noise":noise}})).unwrap()
}

fn number(c: &Connection, sql: &str) -> i64 {
    c.query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn hot_issues_take_the_disk_of_their_sample_not_of_every_payload() {
    let c = memory();
    // 10 issues of 501 events each, every event in a transaction of its own,
    // keep 56 payloads each: the first 50, every 100th, and the newest.
    let issues = ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"];
    for n in 0..501 {
        for issue in issues {
            store::insert(&c, 1, &event(issue, n)).unwrap();
        }
    }
    let bytes = number(
        &c,
        "SELECT page_count * page_size FROM pragma_page_count, pragma_page_size",
    );
    // 560 payloads of about 2 KB and 5,010 small rows take 2.2 MB. A
    // payload written into the events row and cleared by the issue's next
    // event left a hole of 2 KB in a page SQLite did not write again, and
    // the store took 11 MB.
    assert!(bytes < 3_000_000, "{bytes} bytes");
    let newest = store::payload(&c, 10).unwrap().unwrap();
    assert_eq!(newest["extra"]["n"], 500);
}

#[test]
fn the_newest_payload_lives_outside_the_events_row() {
    let c = memory();
    for n in 0..52 {
        store::insert(&c, 1, &event("boom", n)).unwrap();
    }
    assert_eq!(number(&c, "SELECT count(payload) FROM events"), 50);
    assert_eq!(number(&c, "SELECT event FROM latest_payloads"), 52);
    assert_eq!(store::payload(&c, 1).unwrap().unwrap()["extra"]["n"], 51);
}

#[test]
fn the_migration_moves_a_marked_newest_payload_out_of_events() {
    let mut c = memory();
    for n in 0..51 {
        store::insert(&c, 1, &event("boom", n)).unwrap();
    }
    // A second issue whose newest event is in the sample.
    let other = ErrorEvent::parse(json!({"message":"other"})).unwrap();
    store::insert(&c, 1, &other).unwrap();
    // Put the store back as the version before left it: the newest payload outside
    // the sample in its events row, behind an empty zstd skippable frame.
    let (event, payload): (i64, Vec<u8>) = c
        .query_row("SELECT event, payload FROM latest_payloads", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    let marked = [&[0x5D, 0x2A, 0x4D, 0x18, 0, 0, 0, 0], payload.as_slice()].concat();
    c.execute(
        "UPDATE events SET payload=?2 WHERE id=?1",
        rusqlite::params![event, marked],
    )
    .unwrap();
    c.execute_batch(&format!(
        "DROP TABLE latest_payloads; {LATER} PRAGMA user_version={BEFORE};"
    ))
    .unwrap();
    store::init(&mut c).unwrap();
    assert_eq!(number(&c, "PRAGMA user_version"), version(LAST));
    assert_eq!(number(&c, "SELECT count(payload) FROM events"), 51);
    let (issue, event, mark): (i64, i64, Vec<u8>) = c
        .query_row(
            "SELECT issue_id, event, substr(payload, 1, 8) FROM latest_payloads",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!((issue, event), (1, 51));
    assert_eq!(
        mark[..4],
        [0x28, 0xB5, 0x2F, 0xFD],
        "a zstd frame, unmarked"
    );
    assert_eq!(store::payload(&c, 1).unwrap().unwrap()["extra"]["n"], 50);
    assert_eq!(store::payload(&c, 2).unwrap().unwrap()["message"], "other");
}

#[test]
fn a_sampled_newest_event_takes_the_row_away() {
    let c = memory();
    for n in 0..99 {
        store::insert(&c, 1, &event("boom", n)).unwrap();
    }
    assert_eq!(number(&c, "SELECT event FROM latest_payloads"), 99);
    // The 100th event is in the sample, so it keeps its own payload.
    store::insert(&c, 1, &event("boom", 99)).unwrap();
    assert_eq!(number(&c, "SELECT count(*) FROM latest_payloads"), 0);
    assert_eq!(number(&c, "SELECT count(payload) FROM events"), 51);
    assert_eq!(store::payload(&c, 1).unwrap().unwrap()["extra"]["n"], 99);
}

#[test]
fn a_transaction_that_ends_on_a_sampled_event_leaves_no_row() {
    let mut c = memory();
    for n in 0..98 {
        store::insert(&c, 1, &event("boom", n)).unwrap();
    }
    assert_eq!(number(&c, "SELECT event FROM latest_payloads"), 98);
    // Events 99 and 100 in one transaction: 99 is outside the sample, 100
    // in it, and the newest decides.
    let job = store::Job::Ingest {
        project: 1,
        events: vec![event("boom", 98), event("boom", 99)],
        held: crate::http::body::Budget::new(0).hold(),
    };
    let done = tokio::sync::oneshot::channel().0;
    let outcomes = store::write_batch(&mut c, &[store::Write { job, done }]);
    assert!(matches!(outcomes[..], [Ok(())]));
    assert_eq!(number(&c, "SELECT count(*) FROM latest_payloads"), 0);
    assert_eq!(store::payload(&c, 1).unwrap().unwrap()["extra"]["n"], 99);
}

/// Puts the empty zstd skippable frame of the version before in front of the payload
/// of event `id`.
fn mark(c: &Connection, id: i64, payload: &[u8]) {
    let marked = [&[0x5D, 0x2A, 0x4D, 0x18, 0, 0, 0, 0], payload].concat();
    c.execute(
        "UPDATE events SET payload=?2 WHERE id=?1",
        rusqlite::params![id, marked],
    )
    .unwrap();
}

/// A store as the version before left it: issue 1 of 51 events, the newest one's
/// payload in its events row behind the mark.
fn behind() -> Connection {
    let c = memory();
    for n in 0..51 {
        store::insert(&c, 1, &event("boom", n)).unwrap();
    }
    let payload: Vec<u8> = c
        .query_row("SELECT payload FROM latest_payloads", [], |r| r.get(0))
        .unwrap();
    mark(&c, 51, &payload);
    c.execute_batch(&format!(
        "DROP TABLE latest_payloads; {LATER} PRAGMA user_version={BEFORE};"
    ))
    .unwrap();
    c
}

#[test]
fn a_migration_that_fails_moves_nothing_and_runs_again() {
    let mut c = behind();
    c.execute_batch(
        "CREATE TRIGGER refuse BEFORE UPDATE OF payload ON events
         BEGIN SELECT RAISE(ABORT, 'refused'); END;",
    )
    .unwrap();
    // The last statement fails, after the table and its row were written.
    let e = store::init(&mut c).unwrap_err();
    assert_eq!(e.to_string(), "refused");
    assert_eq!(number(&c, "PRAGMA user_version"), version(BEFORE));
    let tables = "SELECT count(*) FROM sqlite_schema WHERE name='latest_payloads'";
    assert_eq!(number(&c, tables), 0);
    let marked = "SELECT count(*) FROM events WHERE substr(payload, 1, 8)=x'5D2A4D1800000000'";
    assert_eq!(number(&c, marked), 1);
    c.execute_batch("DROP TRIGGER refuse").unwrap();
    store::init(&mut c).unwrap();
    assert_eq!(number(&c, "PRAGMA user_version"), version(LAST));
    assert_eq!(number(&c, marked), 0);
    assert_eq!(number(&c, "SELECT event FROM latest_payloads"), 51);
    assert_eq!(store::payload(&c, 1).unwrap().unwrap()["extra"]["n"], 50);
}

#[test]
fn the_migration_leaves_a_marked_payload_of_an_older_event() {
    let mut c = behind();
    let payload: Vec<u8> = c
        .query_row("SELECT payload FROM events WHERE id=1", [], |r| r.get(0))
        .unwrap();
    mark(&c, 1, &payload);
    store::init(&mut c).unwrap();
    assert_eq!(number(&c, "SELECT count(*) FROM latest_payloads"), 1);
    assert_eq!(number(&c, "SELECT event FROM latest_payloads"), 51);
    assert_eq!(number(&c, "SELECT count(payload) FROM events"), 50);
    // zstd skips the empty frame of the mark, so the payload still reads.
    let blob: Vec<u8> = c
        .query_row("SELECT payload FROM events WHERE id=1", [], |r| r.get(0))
        .unwrap();
    let raw: serde_json::Value =
        serde_json::from_slice(&zstd::decode_all(blob.as_slice()).unwrap()).unwrap();
    assert_eq!(raw["extra"]["n"], 0);
}

#[test]
fn the_migration_moves_the_newest_payload_of_each_project_and_passes_an_empty_issue() {
    let mut c = memory();
    store::create_project(&c, "api").unwrap();
    // The same error in two projects is two issues, 1 and 2.
    for project in [1, 2] {
        for n in 0..51 {
            store::insert(&c, project, &event("boom", n)).unwrap();
        }
    }
    // Issue 3 kept no event, as when retention deleted them all but feedback
    // still points at it.
    let gone = ErrorEvent::parse(json!({"message":"gone"})).unwrap();
    store::insert(&c, 1, &gone).unwrap();
    c.execute("DELETE FROM events WHERE issue_id=3", [])
        .unwrap();
    let rows: Vec<(i64, Vec<u8>)> = c
        .prepare("SELECT event, payload FROM latest_payloads")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    for (event, payload) in &rows {
        mark(&c, *event, payload);
    }
    c.execute_batch(&format!(
        "DROP TABLE latest_payloads; {LATER} PRAGMA user_version={BEFORE};"
    ))
    .unwrap();
    store::init(&mut c).unwrap();
    let moved: Vec<String> = c
        .prepare("SELECT issue_id || ':' || event FROM latest_payloads ORDER BY issue_id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(moved, ["1:51", "2:102"]);
    assert_eq!(number(&c, "SELECT count(payload) FROM events"), 100);
    assert_eq!(store::payload(&c, 2).unwrap().unwrap()["extra"]["n"], 50);
    assert_eq!(store::payload(&c, 3).unwrap(), None);
}
