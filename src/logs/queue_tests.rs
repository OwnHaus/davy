use super::*;

fn record(message: &str) -> LogRecord {
    LogRecord {
        project_id: 1,
        timestamp: 1,
        observed_at: 1,
        source: LogSource::Logtail,
        level: LogLevel::Info,
        message: message.into(),
        service: None,
        trace_id: None,
        span_id: None,
        attributes: Map::new(),
    }
}

#[test]
fn the_log_queue_is_full_at_its_row_or_byte_limit() {
    let big = || vec![record(&"x".repeat(600_000))];
    let (queue, inbox) = LogQueue::bounded(1000, 1_000_000);
    assert!(queue.push(big()).is_ok());
    assert!(matches!(queue.push(big()), Err(QueueError::Full)));
    assert_eq!(inbox.try_recv().map(|b| b.len()), Some(1));
    assert!(queue.push(big()).is_ok(), "a written batch frees its bytes");

    let small = || vec![record("x")];
    let (queue, _inbox) = LogQueue::bounded(2, 1_000_000);
    assert!(queue.push(small()).is_ok());
    assert!(queue.push(small()).is_ok());
    assert!(matches!(queue.push(small()), Err(QueueError::Full)));
}

#[test]
fn a_batch_larger_than_the_queue_is_refused_even_when_it_is_empty() {
    let records = |n, text: &str| vec![record(text); n];
    let (queue, inbox) = LogQueue::bounded(2, 1_000_000);
    assert!(matches!(
        queue.push(records(3, "x")),
        Err(QueueError::TooLarge)
    ));
    let long = "x".repeat(1_000_000);
    assert!(matches!(
        queue.push(records(1, &long)),
        Err(QueueError::TooLarge)
    ));
    assert!(
        queue.push(records(2, "x")).is_ok(),
        "a refusal holds no room"
    );
    assert_eq!(inbox.try_recv().map(|b| b.len()), Some(2));
}

#[test]
fn a_batch_holds_its_room_until_the_writer_is_done_with_it() {
    let one = || vec![record("x")];
    let (queue, inbox) = LogQueue::bounded(1, 1_000_000);
    assert!(queue.push(one()).is_ok());
    let (batch, held) = inbox.recv_timeout(Duration::ZERO).unwrap();
    assert!(
        matches!(queue.push(one()), Err(QueueError::Full)),
        "received is not written"
    );
    drop((batch, held));
    assert!(queue.push(one()).is_ok());
}
