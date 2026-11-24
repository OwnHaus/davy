//! A sweep and the newest payload of an issue, which `latest_payloads`
//! holds when its event is outside the sample.
use super::*;

/// Writes `late` and a sweep at day 20 in one writer batch, as when the
/// server clock moved on by more than the retention between taking the
/// events in and queueing the sweep.
fn ingest_and_sweep(s: &Server, late: Vec<ErrorEvent>) -> Vec<store::Outcome> {
    let write = |job| Write {
        job,
        done: tokio::sync::oneshot::channel().0,
    };
    let held = crate::http::body::Budget::new(0).hold();
    let batch = [
        write(Job::Ingest {
            project: 1,
            events: late,
            held,
        }),
        write(Job::Sweep {
            at: day(20),
            error_days: Days::new(10).unwrap(),
        }),
    ];
    store::write_batch(&mut store::open(&s.root).unwrap(), &batch)
}

#[test]
fn an_event_the_same_batch_expires_leaves_the_newest_payload_before_it() {
    let s = Server::new("errors-same-batch", &["api"]);
    // Events 1 to 51 stay, the newest outside the sample.
    let timeouts = (0..51).map(|n| error("timeout", day(15) + n, "prod", "v1"));
    s.ingest(1, timeouts.collect());
    let late = vec![error("timeout", day(1), "prod", "v1")];
    assert_eq!(ingest_and_sweep(&s, late), [Ok(()), Ok(())]);
    assert_eq!(s.number("SELECT count(*) FROM events"), 51);
    let row = s.rows("SELECT issue_id || ':' || event FROM latest_payloads");
    assert_eq!(row, ["1:51"]);
}

#[test]
fn an_issue_the_same_batch_expires_does_not_fail_the_batch() {
    let s = Server::new("errors-same-batch-issue", &["api"]);
    let hangs = (0..51).map(|n| error("hang", day(1) + n, "prod", "v1"));
    s.ingest(1, hangs.collect());
    let late = vec![error("hang", day(1) + 60, "prod", "v1")];
    assert_eq!(ingest_and_sweep(&s, late), [Ok(()), Ok(())]);
    assert_eq!(s.number("SELECT count(*) FROM issues"), 0);
    assert_eq!(s.number("SELECT count(*) FROM latest_payloads"), 0);
}

#[test]
fn a_newest_payload_row_naming_another_issues_event_goes_with_its_issue() {
    let s = Server::new("errors-stray", &["api"]);
    let timeouts = (0..51).map(|n| error("timeout", day(1) + n, "prod", "v1"));
    s.ingest(1, timeouts.collect());
    s.ingest(1, vec![error("hang", day(15), "prod", "v1")]);
    // The row of timeout names event 52 of hang, as a row left behind by
    // an older Davy does once its event's id was given again.
    s.c.execute("UPDATE latest_payloads SET event=52 WHERE issue_id=1", [])
        .unwrap();
    s.sweep(day(20), 10);
    assert_eq!(s.rows("SELECT title FROM issues"), ["hang"]);
    assert_eq!(s.number("SELECT count(*) FROM latest_payloads"), 0);
}
