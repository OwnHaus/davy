use super::{NewMonitor, Target, checkin_store_tests::memory, retention, store};
use rusqlite::Connection;
use std::time::Duration;

/// 31 days after the epoch, so rows at 0 are past the 30 days kept.
const NOW: i64 = 2_678_400_000;

/// 2,500 expired checks and resolved incidents, 10 recent checks and one
/// open incident.
fn backlog() -> Connection {
    let c = memory();
    let heartbeat = Target::Heartbeat {
        grace: Duration::ZERO,
        slug: None,
    };
    let m = store::add(&c, 1, &NewMonitor::new("job", heartbeat, None).unwrap(), 0).unwrap();
    c.execute_batch(&format!(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<2500)
         INSERT INTO checks(monitor_id,at,ok) SELECT {id},0,1 FROM n;
         WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<2500)
         INSERT INTO incidents(monitor_id,started_at,resolved_at,reason) SELECT {id},0,1,'late' FROM n;
         WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<10)
         INSERT INTO checks(monitor_id,at,ok) SELECT {id},{NOW}-i,1 FROM n;
         INSERT INTO incidents(monitor_id,started_at,reason) VALUES ({id},0,'late');",
        id = m.id
    ))
    .unwrap();
    c
}

fn left(c: &Connection) -> (i64, i64) {
    c.query_row(
        "SELECT (SELECT count(*) FROM checks),(SELECT count(*) FROM incidents)",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .unwrap()
}

#[test]
fn a_sweep_with_time_left_deletes_every_expired_batch() {
    let c = backlog();
    retention::prune(&c, NOW, Duration::from_hours(1)).unwrap();
    assert_eq!(left(&c), (10, 1));
}

#[test]
fn a_spent_budget_stops_after_one_batch_per_class() {
    let c = backlog();
    retention::prune(&c, NOW, Duration::ZERO).unwrap();
    assert_eq!(left(&c), (1510, 1501));
    retention::prune(&c, NOW, Duration::ZERO).unwrap();
    assert_eq!(left(&c), (510, 501));
}
