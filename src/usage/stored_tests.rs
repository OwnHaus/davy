use super::*;
use crate::{
    event::ErrorEvent,
    logs::store::log_days::{LogDays, Row, Segment},
    store,
};
use serde_json::json;

/// 2026-11-23T00:00:00Z in unix milliseconds.
const NOV_23: i64 = 1_795_392_000_000;
const DAY_MS: i64 = 86_400_000;

fn memory() -> Connection {
    let mut c = Connection::open_in_memory().unwrap();
    store::init(&mut c).unwrap();
    store::create_project(&c, "api").unwrap();
    store::create_project(&c, "shop").unwrap();
    c
}

fn day(s: &str) -> Date {
    s.parse().unwrap()
}

fn rows(rows: Vec<Row>) -> Flush {
    Flush {
        start: None,
        segments: vec![Segment {
            rows,
            marks: Vec::new(),
        }],
    }
}

fn row(d: &str, project: i64, records: u64, bytes: u64) -> Row {
    Row {
        day: day(d),
        project,
        count: Count { records, bytes },
    }
}

fn number(c: &Connection, sql: &str) -> i64 {
    c.query_row(sql, [], |r| r.get(0)).unwrap()
}

fn store_days(c: &Connection) -> Vec<(String, i64, i64, i64)> {
    let mut q = c
        .prepare("SELECT day, at, bytes, events FROM store_days ORDER BY day")
        .unwrap();
    q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn writes_add_to_the_days_rows_and_skip_unknown_projects() {
    let c = memory();
    let flush = rows(vec![
        row("2026-11-23", 1, 2, 30),
        row("2026-11-23", 3, 1, 10),
    ]);
    write(&c, &flush).unwrap();
    write(&c, &rows(vec![row("2026-11-23", 1, 1, 5)])).unwrap();
    let got = between(&c, day("2026-11-23"), day("2026-11-23"), None).unwrap();
    assert_eq!(
        got,
        HashMap::from([(
            1,
            Count {
                records: 3,
                bytes: 35
            }
        )])
    );
    assert_eq!(number(&c, "SELECT count(*) FROM log_days"), 1);
}

#[test]
fn counting_start_is_recorded_once() {
    let c = memory();
    let log_days = LogDays::new(NOV_23);
    let first = log_days.take();
    assert_eq!(first.start, Some(NOV_23));
    assert_eq!(log_days.take().start, None, "taken once");
    write(&c, &first).unwrap();
    // A server started later counts from then, but the first start stays.
    let later = Flush {
        start: Some(NOV_23 + DAY_MS),
        segments: Vec::new(),
    };
    write(&c, &later).unwrap();
    let at = "SELECT at FROM firsts WHERE what='log_days'";
    assert_eq!(number(&c, at), NOV_23);
}

#[test]
fn a_cut_row_stays_cut() {
    let c = memory();
    let d = day("2026-11-20");
    let flush = Flush {
        start: None,
        segments: vec![
            Segment {
                rows: vec![row("2026-11-20", 1, 1, 1), row("2026-11-20", 2, 1, 1)],
                marks: vec![Mark::Only(d, vec![1])],
            },
            Segment {
                rows: vec![row("2026-11-20", 1, 1, 1)],
                marks: vec![Mark::AllBut(d, vec![1, 2])],
            },
        ],
    };
    write(&c, &flush).unwrap();
    let cut = "SELECT group_concat(cut) FROM (SELECT cut FROM log_days ORDER BY project_id)";
    let cut: String = c.query_row(cut, [], |r| r.get(0)).unwrap();
    assert_eq!(cut, "1,0");
}

#[test]
fn store_days_samples_live_pages_and_events_once_a_day() {
    let c = memory();
    for message in ["boom", "boom", "bang"] {
        let e = ErrorEvent::parse(json!({"message": message})).unwrap();
        store::insert(&c, 1, &e).unwrap();
    }
    let live = number(&c, "PRAGMA page_count") - number(&c, "PRAGMA freelist_count");
    let bytes = live * number(&c, "PRAGMA page_size");
    sample(&c, NOV_23 + 5).unwrap();
    let e = ErrorEvent::parse(json!({"message": "later"})).unwrap();
    store::insert(&c, 2, &e).unwrap();
    sample(&c, NOV_23 + DAY_MS - 1).unwrap();
    assert_eq!(
        store_days(&c),
        [("2026-11-23".to_owned(), NOV_23 + 5, bytes, 3)]
    );
    sample(&c, NOV_23 + DAY_MS).unwrap();
    let next = store_days(&c).pop().unwrap();
    assert_eq!((next.0.as_str(), next.3), ("2026-11-24", 4));
}

#[test]
fn store_days_count_live_pages_not_free_ones() {
    let c = memory();
    let filler = "x".repeat(100_000);
    c.execute("CREATE TABLE filler(v TEXT)", []).unwrap();
    for _ in 0..20 {
        c.execute("INSERT INTO filler VALUES (?1)", [&filler])
            .unwrap();
    }
    c.execute("DELETE FROM filler", []).unwrap();
    assert!(number(&c, "PRAGMA freelist_count") > 400);
    let live = number(&c, "PRAGMA page_count") - number(&c, "PRAGMA freelist_count");
    sample(&c, NOV_23).unwrap();
    let bytes = number(&c, "SELECT bytes FROM store_days");
    assert_eq!(bytes, live * number(&c, "PRAGMA page_size"));
}

#[test]
fn log_days_and_store_days_are_pruned_after_400_days() {
    let c = memory();
    let flush = rows(vec![row("2025-10-19", 1, 1, 1), row("2025-10-20", 1, 1, 1)]);
    write(&c, &flush).unwrap();
    // 2026-11-23 is 400 days after 2025-10-19.
    let old = NOV_23 - 400 * DAY_MS;
    sample(&c, old).unwrap();
    sample(&c, old + DAY_MS).unwrap();
    prune(&c, NOV_23).unwrap();
    let days = "SELECT group_concat(day) FROM log_days";
    let days: String = c.query_row(days, [], |r| r.get(0)).unwrap();
    assert_eq!(days, "2025-10-20");
    let kept: Vec<String> = store_days(&c).into_iter().map(|r| r.0).collect();
    assert_eq!(kept, ["2025-10-20"]);
}

#[test]
fn the_sweep_deletes_at_most_10000_old_log_days_at_a_time() {
    let c = memory();
    let last = day("2025-10-19");
    let old: Vec<Row> = (0..5001)
        .flat_map(|i| {
            let d = last.checked_sub(i.days()).unwrap();
            [1, 2].map(|project| Row {
                day: d,
                project,
                count: Count::new(1, 1),
            })
        })
        .collect();
    write(&c, &rows(old)).unwrap();
    assert_eq!(number(&c, "SELECT count(*) FROM log_days"), 10_002);
    prune(&c, NOV_23).unwrap();
    assert_eq!(number(&c, "SELECT count(*) FROM log_days"), 2);
    prune(&c, NOV_23).unwrap();
    assert_eq!(number(&c, "SELECT count(*) FROM log_days"), 0);
}

#[test]
fn a_sweep_job_samples_and_prunes() {
    let mut c = memory();
    write(&c, &rows(vec![row("2025-10-19", 1, 1, 1)])).unwrap();
    let sweep = store::Write {
        job: store::Job::Sweep {
            at: NOV_23,
            error_days: crate::retention::Days::new(30).unwrap(),
        },
        done: tokio::sync::oneshot::channel().0,
    };
    assert_eq!(store::write_batch(&mut c, &[sweep]), [Ok(())]);
    assert_eq!(number(&c, "SELECT count(*) FROM log_days"), 0);
    assert_eq!(number(&c, "SELECT count(*) FROM store_days"), 1);
}

#[test]
fn the_migration_adds_both_tables_to_a_store_of_version_19() {
    let mut c = memory();
    write(&c, &rows(vec![row("2026-11-23", 1, 1, 1)])).unwrap();
    c.execute_batch(&format!("{UNDO} PRAGMA user_version=19;"))
        .unwrap();
    let tables = "SELECT count(*) FROM sqlite_master WHERE name IN ('log_days', 'store_days')";
    assert_eq!(number(&c, tables), 0);
    store::init(&mut c).unwrap();
    assert_eq!(number(&c, tables), 2);
    assert_eq!(number(&c, "SELECT count(*) FROM log_days"), 0);
    assert_eq!(number(&c, "PRAGMA user_version"), 20);
    assert_eq!(store::SCHEMA_VERSION, 20);
}

/// The next job the flush puts in `inbox`.
async fn next(inbox: &crossbeam_channel::Receiver<store::Write>) -> store::Write {
    loop {
        if let Ok(write) = inbox.try_recv() {
            return write;
        }
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn stored_logs_go_after_the_counts_and_come_back_when_the_writer_fails() {
    let (sender, inbox) = crossbeam_channel::bounded(1);
    let counter = crate::usage::Counter::default();
    counter.add(1, crate::usage::Signal::Logs, Count::new(1, 10));
    let stored = LogDays::new(1_795_392_000_000);
    let flush = tokio::spawn({
        let (counter, stored) = (counter.clone(), stored.clone());
        async move { crate::usage::flush(&counter, &stored, &sender).await }
    });
    let write = next(&inbox).await;
    assert!(matches!(&write.job, store::Job::Usage(rows) if rows.len() == 1));
    write.done.send(Ok(())).unwrap();
    let write = next(&inbox).await;
    let store::Job::LogDays(sent) = &write.job else {
        panic!("a log_days job");
    };
    let sent = sent.clone();
    assert_eq!(sent.start, Some(1_795_392_000_000));
    write.done.send(Err("disk I/O error".into())).unwrap();
    let refused = flush.await.unwrap().unwrap_err();
    assert_eq!(refused.to_string(), "disk I/O error");
    assert_eq!(stored.take(), sent, "put back for the next flush");
}
