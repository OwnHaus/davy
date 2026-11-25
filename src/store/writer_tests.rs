use super::*;
use crate::store::{create_project, open};

fn job(n: i64) -> (Write, oneshot::Receiver<Outcome>) {
    let (done, outcome) = oneshot::channel();
    let job = Job::Sweep {
        at: n,
        error_days: crate::retention::Defaults::default().errors,
    };
    (Write { job, done }, outcome)
}

#[test]
fn a_job_from_a_person_goes_first_while_ingest_is_full() {
    let (ingest_tx, ingest) = crossbeam_channel::bounded(2);
    let (ui_tx, ui) = crossbeam_channel::bounded(UI_QUEUE);
    for n in 1..=2 {
        ingest_tx.try_send(job(n).0).unwrap();
    }
    assert!(ingest_tx.try_send(job(3).0).is_err(), "ingest is full");
    ui_tx.try_send(job(99).0).unwrap();
    let batch = Queues::new(ui, ingest).next_batch().unwrap();
    let order: Vec<_> = batch
        .iter()
        .map(|w| match w.job {
            Job::Sweep { at, .. } => at,
            _ => 0,
        })
        .collect();
    assert_eq!(order, [99, 1, 2]);
}

#[test]
fn people_who_keep_writing_leave_half_of_each_batch_to_ingest() {
    let (ingest_tx, ingest) = crossbeam_channel::bounded(BATCH);
    let (ui_tx, ui) = crossbeam_channel::bounded(UI_QUEUE);
    for n in 0..64 {
        ui_tx.try_send(job(1000 + n).0).unwrap();
        ingest_tx.try_send(job(n).0).unwrap();
    }
    let batch = Queues::new(ui, ingest).next_batch().unwrap();
    let people = batch
        .iter()
        .filter(|w| matches!(w.job, Job::Sweep { at, .. } if at >= 1000))
        .count();
    assert_eq!((batch.len(), people), (64, 32));
}

#[test]
fn a_batch_that_reads_first_waits_for_a_cli_write_instead_of_failing() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static WAITED: AtomicBool = AtomicBool::new(false);
    fn waiting(_tries: i32) -> bool {
        WAITED.store(true, Ordering::SeqCst);
        thread::yield_now();
        true
    }
    let root = crate::test_support::scratch("store-batch-busy");
    let mut writer = open(&root).unwrap();
    writer.busy_handler(Some(waiting)).unwrap();
    // The CLI, as `davy resolve`, writing from a process of its own.
    let cli = open(&root).unwrap();
    cli.execute_batch("BEGIN IMMEDIATE; INSERT INTO id_floor(name, id) VALUES ('cli', 1);")
        .unwrap();
    // A sweep reads the late monitors before it writes.
    let batch = thread::spawn(move || write_batch(&mut writer, &[job(0).0]));
    while !WAITED.load(Ordering::SeqCst) && !batch.is_finished() {
        thread::yield_now();
    }
    cli.execute_batch("COMMIT").unwrap();
    assert_eq!(batch.join().unwrap(), [Ok(())]);
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn a_retention_change_waits_while_the_log_writer_deletes() {
    use crate::retention::{By, Change, Setting, log_days};
    let root = crate::test_support::scratch("store-retention-lock");
    let c = open(&root).unwrap();
    let (api, _) = create_project(&c, "api").unwrap();
    let writer = Writer::start(open(&root).unwrap(), 8);
    let deleting = writer.retention.hold();
    let change = Change {
        project: "api".into(),
        logs_days: Some(Setting::Days(2)),
        errors_days: None,
    };
    let (done, outcome) = oneshot::channel();
    let job = Job::SetRetention(change, By::Person);
    writer.ui.send(Write { job, done }).unwrap();
    let (saved, waited) = std::sync::mpsc::channel();
    thread::spawn(move || saved.send(outcome.blocking_recv()).unwrap());
    let early = waited.recv_timeout(std::time::Duration::from_millis(300));
    assert!(early.is_err(), "saved during a deletion: {early:?}");
    assert!(log_days(&c).unwrap().is_empty());
    drop(deleting);
    assert_eq!(waited.recv().unwrap().unwrap(), Ok(()));
    let saved = std::collections::HashMap::from([(api.cast_unsigned(), 2)]);
    assert_eq!(log_days(&c).unwrap(), saved);
    drop((writer.sender, writer.ui));
    writer.thread.join().unwrap();
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn a_usage_write_that_fails_leaves_the_rest_of_its_batch_stored() {
    let mut c = Connection::open_in_memory().unwrap();
    super::super::init(&mut c).unwrap();
    create_project(&c, "api").unwrap();
    c.execute_batch(crate::usage::UNDO).unwrap();
    let first = Job::First {
        what: crate::firsts::First::Log,
        at: 1_795_392_000_000,
    };
    let count = Job::Usage(vec![crate::usage::Row {
        day: "2026-11-23".parse().unwrap(),
        project: 1,
        signal: crate::usage::Signal::Logs,
        count: crate::usage::Count::new(1, 10),
    }]);
    let batch: Vec<Write> = [first, count]
        .into_iter()
        .map(|job| Write {
            job,
            done: oneshot::channel().0,
        })
        .collect();
    let outcomes = write_batch(&mut c, &batch);
    assert_eq!(
        outcomes,
        [Ok(()), Err("no such table: usage_daily".to_owned())]
    );
    let firsts: i64 = c
        .query_row("SELECT count(*) FROM firsts WHERE what='log'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(firsts, 1);
}

#[test]
fn a_log_days_write_that_fails_leaves_the_rest_of_its_batch_stored() {
    let mut c = Connection::open_in_memory().unwrap();
    super::super::init(&mut c).unwrap();
    create_project(&c, "api").unwrap();
    c.execute_batch(crate::usage::stored::UNDO).unwrap();
    let first = Job::First {
        what: crate::firsts::First::Log,
        at: 1_795_392_000_000,
    };
    let stored = Job::LogDays(crate::logs::store::log_days::Flush {
        start: Some(1_795_392_000_000),
        segments: vec![crate::logs::store::log_days::Segment {
            rows: vec![crate::logs::store::log_days::Row {
                day: "2026-11-23".parse().unwrap(),
                project: 1,
                count: crate::usage::Count::new(1, 10),
            }],
            marks: Vec::new(),
        }],
    });
    let batch: Vec<Write> = [first, stored]
        .into_iter()
        .map(|job| Write {
            job,
            done: oneshot::channel().0,
        })
        .collect();
    let outcomes = write_batch(&mut c, &batch);
    assert_eq!(
        outcomes,
        [Ok(()), Err("no such table: log_days".to_owned())]
    );
    let firsts: Vec<String> = c
        .prepare("SELECT what FROM firsts")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    // The start of counting went back with its job.
    assert_eq!(firsts, ["log"]);
}

#[test]
fn a_search_write_that_fails_leaves_the_rest_of_its_batch_stored() {
    use crate::usage::savings::searches::{By, Search};
    let mut c = Connection::open_in_memory().unwrap();
    super::super::init(&mut c).unwrap();
    create_project(&c, "api").unwrap();
    c.execute_batch("DROP TABLE search_days").unwrap();
    let first = Job::First {
        what: crate::firsts::First::Log,
        at: 1_795_392_000_000,
    };
    let search = Job::Search {
        at: 1_795_392_000_000,
        search: Search {
            by: By::Agent,
            project: Some(1),
            since: 0,
            until: 1,
            level: None,
            pattern: None,
            query: None,
        },
    };
    let batch: Vec<Write> = [first, search]
        .into_iter()
        .map(|job| Write {
            job,
            done: oneshot::channel().0,
        })
        .collect();
    let outcomes = write_batch(&mut c, &batch);
    assert_eq!(
        outcomes,
        [Ok(()), Err("no such table: search_days".to_owned())]
    );
    let rows: (i64, i64) = c
        .query_row(
            "SELECT (SELECT count(*) FROM firsts WHERE what='log'), (SELECT count(*) FROM searches)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    // The first log stays, and the search went back with its own job.
    assert_eq!(rows, (1, 0));
}
