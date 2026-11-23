use super::*;

const DAY: Timestamp = 20_000 * DAY_MS;

fn memory() -> Connection {
    let mut c = Connection::open_in_memory().unwrap();
    crate::store::init(&mut c).unwrap();
    crate::store::create_project(&c, "shop").unwrap();
    c
}

fn d(reason: &str, category: &str, quantity: i64) -> Discarded {
    Discarded {
        reason: reason.to_owned(),
        category: category.to_owned(),
        quantity,
    }
}

fn rows(c: &Connection) -> Vec<(i64, String, String, i64)> {
    let mut q = c
        .prepare("SELECT day, reason, category, quantity FROM client_reports ORDER BY day, reason, category")
        .unwrap();
    q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

#[test]
fn parse_keeps_snake_case_names_and_caps_the_quantity() {
    let payload = br#"{"timestamp": 1700000000.5, "discarded_events": [
        {"reason": "before_send", "category": "error", "quantity": 2},
        {"reason": "Before Send", "category": "error", "quantity": 1},
        {"reason": "queue_overflow", "category": "log_item", "quantity": 0},
        {"reason": "ratelimit_backoff", "category": "", "quantity": 1},
        {"reason": "sample_rate", "category": "transaction", "quantity": 99000000000}
    ]}"#;
    assert_eq!(
        parse(payload),
        Some(vec![
            d("before_send", "error", 2),
            d("sample_rate", "transaction", 1_000_000)
        ])
    );
}

#[test]
fn parse_reads_a_report_without_entries_and_refuses_what_is_not_one() {
    assert_eq!(parse(br#"{"timestamp": 1700000000}"#), Some(vec![]));
    assert_eq!(parse(b"not json"), None);
    assert_eq!(
        parse(br#"{"discarded_events": [{"reason": "x", "category": "y", "quantity": -1}]}"#),
        None
    );
    let long = format!(
        r#"{{"discarded_events": [{{"reason": "{}", "category": "error", "quantity": 1}}]}}"#,
        "a".repeat(65)
    );
    assert_eq!(parse(long.as_bytes()), Some(vec![]));
}

#[test]
fn parse_reads_at_most_a_hundred_entries() {
    let entries: Vec<String> = (0..150)
        .map(|i| format!(r#"{{"reason": "r{i}", "category": "error", "quantity": 1}}"#))
        .collect();
    let payload = format!(r#"{{"discarded_events": [{}]}}"#, entries.join(","));
    let parsed = parse(payload.as_bytes()).unwrap();
    assert_eq!(parsed.len(), 100);
    assert_eq!(parsed[99], d("r99", "error", 1));
}

#[test]
fn add_sums_each_pair_per_utc_day() {
    let c = memory();
    add(
        &c,
        1,
        DAY + 5,
        &[
            d("before_send", "error", 2),
            d("network_error", "log_item", 7),
        ],
    )
    .unwrap();
    add(&c, 1, DAY + DAY_MS - 1, &[d("before_send", "error", 3)]).unwrap();
    add(&c, 1, DAY + DAY_MS, &[d("before_send", "error", 1)]).unwrap();
    assert_eq!(
        rows(&c),
        vec![
            (DAY, "before_send".to_owned(), "error".to_owned(), 5),
            (DAY, "network_error".to_owned(), "log_item".to_owned(), 7),
            (
                DAY + DAY_MS,
                "before_send".to_owned(),
                "error".to_owned(),
                1
            ),
        ]
    );
}

#[test]
fn add_stops_taking_new_pairs_at_the_daily_cap_but_keeps_counting_old_ones() {
    let c = memory();
    let many: Vec<Discarded> = (0..MAX_PAIRS + 5)
        .map(|i| d(&format!("r{i}"), "error", 1))
        .collect();
    add(&c, 1, DAY, &many).unwrap();
    add(&c, 1, DAY, &[d("r0", "error", 4), d("fresh", "error", 1)]).unwrap();
    let r = rows(&c);
    assert_eq!(r.len(), 100);
    assert!(r.contains(&(DAY, "r0".to_owned(), "error".to_owned(), 5)));
    assert!(!r.iter().any(|row| row.1 == "fresh" || row.1 == "r100"));
    add(&c, 1, DAY + DAY_MS, &[d("fresh", "error", 1)]).unwrap();
    assert_eq!(rows(&c).len(), 101);
}

#[test]
fn prune_deletes_the_days_past_ninety() {
    let c = memory();
    for back in [0, 89, 90, 91] {
        add(&c, 1, DAY - back * DAY_MS, &[d("before_send", "error", 1)]).unwrap();
    }
    prune(&c, DAY + 1000).unwrap();
    let days: Vec<i64> = rows(&c).into_iter().map(|r| (DAY - r.0) / DAY_MS).collect();
    assert_eq!(days, vec![90, 89, 0]);
}
