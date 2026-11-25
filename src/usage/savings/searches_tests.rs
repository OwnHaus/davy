use super::*;
use crate::logs::LogLevel;

/// 2026-11-25T10:00:00Z in milliseconds.
const NOW_MS: i64 = 1_795_600_800_000;
const DAY_MS: i64 = time::DAY_NS / 1_000_000;

fn memory() -> Connection {
    let mut c = Connection::open_in_memory().unwrap();
    store::init(&mut c).unwrap();
    c
}

fn query(text: Option<&str>) -> LogQuery {
    LogQuery {
        project: Some(1),
        since: 10,
        until: 20,
        min_level: Some(LogLevel::Warn),
        text: text.map(str::to_owned),
        pattern: Some(u64::MAX),
        limit: 100,
        skip: 0,
    }
}

fn number(c: &Connection, sql: &str) -> i64 {
    c.query_row(sql, [], |r| r.get(0)).unwrap()
}

/// A search of project 1, or of every project, that reads `days` back
/// from `at_ms`.
fn reading(project: Option<i64>, at_ms: i64, days: i64) -> Search {
    Search {
        project,
        since: (at_ms - days * DAY_MS) * 1_000_000,
        until: at_ms * 1_000_000,
        ..Search::of(By::Agent, &query(None))
    }
}

/// The rows of `search_days`, oldest day first.
fn days(c: &Connection) -> Vec<(String, i64, i64, i64)> {
    c.prepare("SELECT day, project_id, searches, oldest_read_days FROM search_days ORDER BY day, project_id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

#[test]
fn a_search_keeps_its_filters_and_a_long_query_is_kept_without_its_text() {
    assert_eq!(
        Search::of(By::Agent, &query(Some("  card declined "))),
        Search {
            by: By::Agent,
            project: Some(1),
            since: 10,
            until: 20,
            level: Some(4),
            pattern: Some(u64::MAX),
            query: Some("card declined".into()),
        }
    );
    assert_eq!(Search::of(By::Ui, &query(Some("  "))).query, None);
    let long = Search::of(By::Ui, &query(Some(&"x".repeat(MAX_QUERY + 1))));
    assert_eq!((long.query, long.since, long.level), (None, 10, Some(4)));
    let edge = "x".repeat(MAX_QUERY);
    assert_eq!(Search::of(By::Ui, &query(Some(&edge))).query, Some(edge));
    // The cap counts bytes: 334 three-byte characters are 1,002 bytes.
    let wide = Search::of(By::Ui, &query(Some(&"€".repeat(334))));
    assert_eq!(wide.query, None);
}

#[test]
fn the_writer_stores_a_search_with_its_pattern_bits_and_its_day() {
    let c = memory();
    let s = Search::of(By::Ui, &query(Some("card")));
    add(&c, NOW_MS, &s).unwrap();
    add(&c, NOW_MS + 1, &s).unwrap();
    let row: (i64, String, i64, i64, i64, i64, i64, String) = c
        .query_row(
            "SELECT at, by, project_id, since, until, level, pattern, query FROM searches WHERE id=1",
            [],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(row, (NOW_MS, "ui".into(), 1, 10, 20, 4, -1, "card".into()));
    // since 10 ns is 20,782 days and 10 hours before NOW_MS: 20,783 rounded up.
    assert_eq!(days(&c), [("2026-11-25".into(), 1, 2, 20_783)]);
}

#[test]
fn a_day_keeps_its_count_and_oldest_read_per_project_and_for_every_project() {
    let c = memory();
    add(&c, NOW_MS, &reading(Some(1), NOW_MS, 3)).unwrap();
    add(&c, NOW_MS + 1, &reading(Some(1), NOW_MS + 1, 60)).unwrap();
    add(&c, NOW_MS + 2, &reading(Some(1), NOW_MS + 2, 1)).unwrap();
    add(&c, NOW_MS, &reading(None, NOW_MS, 14)).unwrap();
    add(&c, NOW_MS + DAY_MS, &reading(Some(1), NOW_MS + DAY_MS, 0)).unwrap();
    assert_eq!(
        days(&c),
        [
            ("2026-11-25".into(), 0, 1, 14),
            ("2026-11-25".into(), 1, 3, 60),
            ("2026-11-26".into(), 1, 1, 0),
        ]
    );
}

#[test]
fn a_full_table_trims_the_search_but_not_its_day_and_the_count_saturates() {
    let c = memory();
    add(&c, NOW_MS, &reading(Some(1), NOW_MS, 60)).unwrap();
    for i in 1..=KEEP_ROWS {
        add(&c, NOW_MS + i, &reading(Some(1), NOW_MS, 1)).unwrap();
    }
    assert_eq!(number(&c, "SELECT min(id) FROM searches"), 2);
    assert_eq!(days(&c), [("2026-11-25".into(), 1, 10_001, 60)]);
    c.execute("UPDATE search_days SET searches=9223372036854775807", [])
        .unwrap();
    add(&c, NOW_MS, &reading(Some(1), NOW_MS, 1)).unwrap();
    assert_eq!(days(&c), [("2026-11-25".into(), 1, i64::MAX, 60)]);
}

#[test]
fn the_table_keeps_the_newest_ten_thousand_searches() {
    let c = memory();
    let s = Search::of(By::Agent, &query(None));
    for i in 0..KEEP_ROWS + 5 {
        add(&c, NOW_MS + i, &s).unwrap();
    }
    assert_eq!(number(&c, "SELECT count(*) FROM searches"), 10_000);
    assert_eq!(number(&c, "SELECT min(id) FROM searches"), 6);
    assert_eq!(number(&c, "SELECT max(id) FROM searches"), 10_005);
}

#[test]
fn the_sweep_deletes_searches_and_days_older_than_ninety_days() {
    let c = memory();
    let s = Search::of(By::Agent, &query(None));
    for at in [NOW_MS - 91 * DAY_MS, NOW_MS - 90 * DAY_MS, NOW_MS - DAY_MS] {
        add(&c, at, &s).unwrap();
    }
    prune(&c, NOW_MS).unwrap();
    let left: Vec<i64> = c
        .prepare("SELECT at FROM searches ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(left, [NOW_MS - 90 * DAY_MS, NOW_MS - DAY_MS]);
    let kept: Vec<String> = days(&c).into_iter().map(|d| d.0).collect();
    assert_eq!(kept, ["2026-08-27", "2026-11-24"]);
}

#[test]
fn a_full_queue_drops_the_search_without_waiting() {
    let (tx, rx) = crossbeam_channel::bounded(1);
    record(&tx, By::Agent, &query(Some("card")));
    record(&tx, By::Agent, &query(Some("second")));
    let first = rx.try_recv().unwrap();
    match first.job {
        Job::Search { search, .. } => assert_eq!(search.query.as_deref(), Some("card")),
        _ => panic!("not a search"),
    }
    assert!(rx.try_recv().is_err(), "the second was dropped");
    record(&tx, By::Agent, &query(Some(&"x".repeat(MAX_QUERY + 1))));
    match rx.try_recv().unwrap().job {
        Job::Search { search, .. } => assert_eq!((search.query, search.since), (None, 10)),
        _ => panic!("not a search"),
    }
}
