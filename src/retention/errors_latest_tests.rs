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

#[test]
fn a_batch_counts_the_newest_payload_in_its_work() {
    let s = Server::new("errors-newest-work", &["api"]);
    // 29 issues of one event, 51 work each, 1,479 in all. Digits in a
    // message are grouped as one, so the issues take letters.
    let letter = |n: i64| char::from(b'a' + u8::try_from(n).unwrap());
    let singles = (0..29)
        .map(|n| {
            let message = format!("hang {}{}", letter(n / 26), letter(n % 26));
            error(&message, day(1) + n, "prod", "v1")
        })
        .collect();
    s.ingest(1, singles);
    // Then one issue of 51 events: 51 for the first, 1 for each of the
    // next 49, 1,579 so far. The newest, outside the sample, has about
    // 240 KB of payload that zstd cannot shrink much, about 58 more.
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    let noise: String = (0..400_000)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            char::from(b'a' + u8::try_from(x % 26).unwrap())
        })
        .collect();
    let mut timeouts: Vec<ErrorEvent> = (0..51)
        .map(|n| error("timeout", day(2) + n, "prod", "v1"))
        .collect();
    timeouts[50].raw["extra"] = json!({ "noise": noise });
    s.ingest(1, timeouts);
    assert_eq!(s.number("SELECT count(*) FROM latest_payloads"), 1);
    sweep(&s.c, day(20), Days::new(10).unwrap(), Duration::ZERO).unwrap();
    // Counted, the newest payload takes the batch past 1,600, so it waits
    // for the next one.
    assert_eq!(s.number("SELECT count(*) FROM events"), 1);
    assert_eq!(s.number("SELECT count(*) FROM latest_payloads"), 1);
}

#[test]
fn an_id_given_again_in_the_same_batch_keeps_the_newest_payload() {
    let s = Server::new("errors-same-batch-reuse", &["api"]);
    let timeouts = (0..51).map(|n| error("timeout", day(15) + n, "prod", "v1"));
    s.ingest(1, timeouts.collect());
    let write = |job| Write {
        job,
        done: tokio::sync::oneshot::channel().0,
    };
    let hold = || crate::http::body::Budget::new(0).hold();
    // Event 52 of timeout expires in the sweep, and hang, a new issue,
    // gets its id back in the same batch.
    let batch = [
        write(Job::Ingest {
            project: 1,
            events: vec![error("timeout", day(1), "prod", "v1")],
            held: hold(),
        }),
        write(Job::Sweep {
            at: day(20),
            error_days: Days::new(10).unwrap(),
        }),
        write(Job::Ingest {
            project: 1,
            events: vec![error("hang", day(19), "prod", "v1")],
            held: hold(),
        }),
    ];
    let outcomes = store::write_batch(&mut store::open(&s.root).unwrap(), &batch);
    assert_eq!(outcomes, [Ok(()), Ok(()), Ok(())]);
    let event = s.rows("SELECT issue_id || ':' || id FROM events WHERE id=52");
    assert_eq!(event, ["2:52"]);
    let row = s.rows("SELECT issue_id || ':' || event FROM latest_payloads");
    assert_eq!(row, ["1:51"]);
}

#[test]
fn a_batch_counts_only_its_own_issues_newest_payload() {
    let s = Server::new("errors-newest-work-pin", &["api"]);
    let timeouts = (0..60).map(|n| error("timeout", day(1) + n, "prod", "v1"));
    s.ingest(1, timeouts.collect());
    s.ingest(1, vec![error("hang", day(15), "prod", "v1")]);
    // A row of hang that names event 55 of timeout, outside the sample,
    // with 6 MiB of payload. Counted for event 55 it would cost 1,537
    // and end the batch there.
    let unsampled = s.number("SELECT payload IS NULL FROM events WHERE id=55");
    assert_eq!(unsampled, 1);
    s.c.execute(
        "INSERT INTO latest_payloads(issue_id, event, payload) VALUES (2, 55, zeroblob(6291456))",
        [],
    )
    .unwrap();
    sweep(&s.c, day(20), Days::new(10).unwrap(), Duration::ZERO).unwrap();
    assert_eq!(s.rows("SELECT title FROM issues"), ["hang"]);
    let rows = s.rows("SELECT issue_id || ':' || event FROM latest_payloads");
    assert_eq!(rows, ["2:55"]);
}
