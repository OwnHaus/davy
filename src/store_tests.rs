use super::*;

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
fn an_upgrade_builds_the_issue_timestamp_index_in_its_migration() {
    let root = crate::test_support::scratch("store-issue-timestamp");
    let c = open(&root).unwrap();
    c.execute_batch("DROP INDEX events_issue_timestamp; PRAGMA user_version=15;")
        .unwrap();
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
    assert_eq!((built, version), (1, 16));
    std::fs::remove_dir_all(&root).unwrap();
}
