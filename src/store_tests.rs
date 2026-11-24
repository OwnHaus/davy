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
