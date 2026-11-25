use crate::{event::ErrorEvent, store};
use rusqlite::Connection;
use serde_json::json;

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
    // Put the store back as version 18 left it: the newest payload outside
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
    c.execute_batch("DROP TABLE latest_payloads; PRAGMA user_version=18;")
        .unwrap();
    store::init(&mut c).unwrap();
    assert_eq!(number(&c, "PRAGMA user_version"), 19);
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
