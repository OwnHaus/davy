use super::{
    searches::{By, Search, add},
    *,
};
use crate::{
    event::ErrorEvent,
    logs::{
        LogRecord, LogSource,
        store::{Examples, LogWriter, MAX_EXAMPLES, no_overrides},
    },
    store,
    test_support::scratch,
    time::DAY_NS,
    usage::{forecast::NoForecast, forecast::WhatIf},
};
use jiff::ToSpan as _;
use std::path::{Path, PathBuf};

const DAY_MS: i64 = DAY_NS / 1_000_000;
const HOUR_MS: i64 = time::HOUR_NS / 1_000_000;
/// 2026-11-25T10:00:00Z in milliseconds, for tests that read no log index.
const NOON_MS: i64 = 1_795_600_800_000;

fn memory() -> Connection {
    let mut c = Connection::open_in_memory().unwrap();
    store::init(&mut c).unwrap();
    store::create_project(&c, "shop").unwrap();
    store::create_project(&c, "api").unwrap();
    c
}

/// A search of `project` at `at_ms` that read from `back_ns` before it.
fn search(project: Option<i64>, back_ns: i64, query: Option<&str>) -> impl Fn(i64) -> Search {
    let query = query.map(str::to_owned);
    move |at_ms| Search {
        by: By::Agent,
        project,
        since: at_ms * 1_000_000 - back_ns,
        until: at_ms * 1_000_000,
        level: None,
        pattern: None,
        query: query.clone(),
    }
}

fn log_day(c: &Connection, day: Date, bytes: i64, cut: bool) {
    c.execute(
        "INSERT INTO log_days(day, project_id, records, bytes, cut) VALUES (?1, 1, 1, ?2, ?3)",
        rusqlite::params![day.to_string(), bytes, cut],
    )
    .unwrap();
}

#[test]
fn retention_fit_suggests_twice_the_oldest_read_of_the_last_30_days() {
    let c = memory();
    let now_ns = NOON_MS * 1_000_000;
    // The first search, 40 days ago, read 80 days back but is past the
    // look-back; its day still dates the history.
    add(
        &c,
        NOON_MS - 40 * DAY_MS,
        &search(Some(1), 80 * DAY_NS, None)(NOON_MS - 40 * DAY_MS),
    )
    .unwrap();
    let at = NOON_MS - 3 * DAY_MS;
    add(&c, at, &search(Some(1), 2 * DAY_NS, Some("payment"))(at)).unwrap();
    // Every project's search counts; 6 days less an hour rounds up to 6.
    add(
        &c,
        at,
        &search(None, 6 * DAY_NS - 3_600_000_000_000, None)(at),
    )
    .unwrap();
    // Another project's search reads far back and does not count.
    add(&c, at, &search(Some(2), 200 * DAY_NS, None)(at)).unwrap();
    let today = time::day_of(now_ns);
    let back = |n: i64| today.checked_sub(n.days()).unwrap();
    // 90 days keeps back(90) to today; 14 days frees back(90) to back(15).
    log_day(&c, back(91), 1_000_000_000, false);
    log_day(&c, back(90), 2_000_000, false);
    log_day(&c, back(89), 10_000_000, false);
    log_day(&c, back(20), 5_000_000, true);
    log_day(&c, back(15), 7, false);
    log_day(&c, back(14), 1_000_000, false);
    let found = fit::fit(&c, (1, "shop"), 90, 500_000, now_ns).unwrap();
    assert_eq!(
        found,
        Some(Fit {
            project: "shop".into(),
            logs_days: 90,
            oldest_read_days: 6,
            searches: 2,
            history_days: 30,
            suggest: 14,
            saves: 6_000_003,
        })
    );
    // Already at 14 days, nothing shorter fits twice 6 days.
    assert_eq!(
        fit::fit(&c, (1, "shop"), 14, 500_000, now_ns).unwrap(),
        None
    );
}

#[test]
fn retention_fit_waits_for_a_week_of_searches_and_one_of_the_project() {
    let c = memory();
    let now_ns = NOON_MS * 1_000_000;
    let a_day_later = now_ns + DAY_NS;
    let today = time::day_of(now_ns);
    log_day(&c, today.checked_sub(20.days()).unwrap(), 2_000_000, false);
    assert_eq!(
        fit::fit(&c, (1, "shop"), 30, 500_000, now_ns).unwrap(),
        None
    );
    // Another project's first search, 7 days ago: 6 whole days since its day.
    let first = NOON_MS - 7 * DAY_MS;
    add(&c, first, &search(Some(2), DAY_NS, None)(first)).unwrap();
    // A week later, still no search of shop or of every project.
    assert_eq!(
        fit::fit(&c, (1, "shop"), 30, 500_000, a_day_later).unwrap(),
        None
    );
    let at = NOON_MS - DAY_MS;
    add(&c, at, &search(Some(1), DAY_NS, None)(at)).unwrap();
    assert_eq!(
        fit::fit(&c, (1, "shop"), 30, 500_000, now_ns).unwrap(),
        None
    );
    assert_eq!(
        fit::fit(&c, (1, "shop"), 30, 500_000, a_day_later).unwrap(),
        Some(Fit {
            project: "shop".into(),
            logs_days: 30,
            oldest_read_days: 1,
            searches: 1,
            history_days: 7,
            suggest: 7,
            saves: 1_000_000,
        })
    );
}

#[test]
fn a_full_search_table_still_remembers_the_deep_read_and_its_days() {
    let c = memory();
    let now_ns = NOON_MS * 1_000_000;
    let today = time::day_of(now_ns);
    for n in 1..=200 {
        log_day(&c, today.checked_sub(n.days()).unwrap(), 1_000_000, false);
    }
    // A short search 31 days ago, then one 10 days ago that read 60 days.
    let first = NOON_MS - 31 * DAY_MS;
    add(
        &c,
        first,
        &search(Some(1), HOUR_MS * 1_000_000, None)(first),
    )
    .unwrap();
    let deep = NOON_MS - 10 * DAY_MS;
    add(&c, deep, &search(Some(1), 60 * DAY_NS, None)(deep)).unwrap();
    // An agent then fills the table with searches of the last minute.
    for i in 0..10_050 {
        let at = NOON_MS - 10_050 + i;
        add(&c, at, &search(None, 60_000_000_000, None)(at)).unwrap();
    }
    let kept: i64 = c
        .query_row(
            "SELECT count(*) FROM searches WHERE at < ?1",
            [NOON_MS - 10_050],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(kept, 0);
    // Twice 60 days is 120: nothing shorter than 90 days fits.
    assert_eq!(
        fit::fit(&c, (1, "shop"), 90, 500_000, now_ns).unwrap(),
        None
    );
    assert_eq!(
        fit::fit(&c, (1, "shop"), 365, 500_000, now_ns).unwrap(),
        Some(Fit {
            project: "shop".into(),
            logs_days: 365,
            oldest_read_days: 60,
            searches: 10_051,
            history_days: 30,
            suggest: 180,
            // back(365) to back(181): 20 days of 1 MB at half.
            saves: 10_000_000,
        })
    );
}

/// An issue of project `project` with `count` events this hour.
fn issue(c: &Connection, project: i64, n: u8, message: &str, count: i64) -> i64 {
    let mut e = ErrorEvent::parse(serde_json::json!({
        "event_id": format!("{n:032x}"),
        "message": message,
    }))
    .unwrap();
    (e.received_at, e.timestamp) = (NOON_MS, NOON_MS);
    store::insert(c, project, &e).unwrap();
    let id: i64 = c
        .query_row("SELECT id FROM issues WHERE title=?1", [message], |r| {
            r.get(0)
        })
        .unwrap();
    c.execute(
        "UPDATE issue_hourly SET count=?2 WHERE issue_id=?1",
        (id, count),
    )
    .unwrap();
    id
}

#[test]
fn noisy_issues_send_a_fifth_of_the_events_and_100_or_more() {
    let mut c = memory();
    let payment = issue(&c, 1, 1, "payment declined", 700);
    issue(&c, 1, 2, "timeout", 300);
    let small = issue(&c, 1, 3, "disk full", 150);
    let gone = issue(&c, 1, 4, "noise", 600);
    crate::discard::set(&mut c, gone, false, NOON_MS).unwrap();
    // The window starts after these: they do not count.
    c.execute(
        "INSERT INTO issue_hourly(issue_id, hour, count) VALUES (?1, ?2, 5000)",
        (small, NOON_MS - 10 * DAY_MS),
    )
    .unwrap();
    // 1750 events: a fifth is 350, so the discarded issue would show.
    let since = NOON_MS - DAY_MS;
    let found = noisy::noisy(&c, (1, "shop"), since, 3).unwrap();
    assert_eq!(
        found,
        [Noisy {
            id: payment,
            project: "shop".into(),
            title: "payment declined".into(),
            events: 700,
            share: 0.4,
        }]
    );
    crate::discard::set(&mut c, gone, true, NOON_MS).unwrap();
    let found = noisy::noisy(&c, (1, "shop"), since, 1).unwrap();
    assert_eq!(found.iter().map(|n| n.id).collect::<Vec<_>>(), [payment]);
    assert_eq!(noisy::noisy(&c, (1, "shop"), since, 3).unwrap().len(), 2);
    // A project of 99 events has no noisy issue, whatever the share.
    issue(&c, 2, 5, "api down", 99);
    assert_eq!(noisy::noisy(&c, (2, "api"), since, 3).unwrap(), []);
}

fn targets() -> [queried::Target<'static>; 3] {
    [
        queried::Target {
            id: 0xaa,
            highest: Some(LogLevel::Info as u8),
            example: "GET /health 200 12ms",
        },
        queried::Target {
            id: 0xbb,
            highest: Some(LogLevel::Error as u8),
            example: "payment declined for order 42",
        },
        queried::Target {
            id: 0xcc,
            highest: Some(LogLevel::Debug as u8),
            example: "cache miss",
        },
    ]
}

#[test]
fn last_queried_is_the_newest_search_whose_filters_find_the_example() {
    let c = memory();
    let mut at = NOON_MS;
    let mut next = |s: Search| {
        at += 1_000;
        add(&c, at, &s).unwrap();
        at
    };
    let base = search(Some(1), DAY_NS, Some("payment"));
    let payment = next(base(0));
    // Above the level of the health pattern's records.
    next(Search {
        level: Some(LogLevel::Warn as u8),
        ..search(Some(1), DAY_NS, Some("health"))(0)
    });
    let cache = next(Search {
        pattern: Some(0xcc),
        ..search(Some(1), DAY_NS, None)(0)
    });
    // Another project, a browse without text or pattern, a text that is
    // not a query and a phrase that is not in the example ask for none.
    next(search(Some(2), DAY_NS, Some("health"))(0));
    next(search(Some(1), DAY_NS, None)(0));
    next(search(Some(1), DAY_NS, Some("health AND"))(0));
    next(search(Some(1), DAY_NS, Some("\"health 404\""))(0));
    let health = next(Search {
        level: Some(LogLevel::Info as u8),
        ..search(None, DAY_NS, Some("health AND 200"))(0)
    });
    let found = queried::last_queried(&c, Some(1), &targets()).unwrap();
    assert_eq!(found, [Some(health), Some(payment), Some(cache)]);
    // For every project, the other project's search is the newest.
    let any = queried::last_queried(&c, None, &targets()).unwrap();
    assert_eq!(any, [Some(health), Some(payment), Some(cache)]);
}

#[test]
fn the_view_reads_at_most_1000_searches_and_indexes_at_most_20_examples() {
    let c = memory();
    add(
        &c,
        NOON_MS,
        &search(Some(1), DAY_NS, Some("payment"))(NOON_MS),
    )
    .unwrap();
    for n in 1..=HISTORY {
        add(
            &c,
            NOON_MS + n,
            &search(Some(1), DAY_NS, Some("unrelated"))(NOON_MS),
        )
        .unwrap();
    }
    let found = queried::last_queried(&c, Some(1), &targets()).unwrap();
    assert_eq!(found, [None, None, None]);
    let examples: Vec<String> = (0..25).map(|n| format!("job {n} done")).collect();
    let index = Examples::new(examples.iter().map(String::as_str)).unwrap();
    assert_eq!(MAX_EXAMPLES, 20);
    assert_eq!(index.found("job"), (0..20).collect::<Vec<_>>());
    assert_eq!(index.found("job AND"), Vec::<usize>::new());
}

/// Logs of 26 patterns in project shop, a second before `now`.
fn write_logs(data: &Path, now: i64) {
    let log = |level, message: &str| LogRecord {
        project_id: 1,
        timestamp: now - 1_000_000_000,
        observed_at: now,
        source: LogSource::Logtail,
        level,
        level_unknown: false,
        message: message.into(),
        service: Some("api".into()),
        trace_id: None,
        span_id: None,
        attributes: serde_json::Map::new(),
    };
    let mut batch: Vec<LogRecord> = (0..100)
        .map(|_| log(LogLevel::Info, "GET /health 200 12ms"))
        .collect();
    batch.extend((0..10).map(|_| log(LogLevel::Error, "payment declined for order 42")));
    for word in [
        "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india",
        "juliett", "kilo", "lima", "mike", "november", "oscar", "papa", "quebec", "romeo",
        "sierra", "tango", "uniform", "victor", "whiskey", "xray",
    ] {
        batch.push(log(LogLevel::Debug, &format!("job {word} done")));
    }
    let writer = LogWriter::start(logs::dir(data), 365, no_overrides()).unwrap();
    assert!(writer.queue.push(batch).is_ok());
    drop(writer.queue);
    writer.thread.join().unwrap();
}

/// A data directory with logs of 26 patterns in project shop, three
/// searches, log days a shorter retention would delete and two noisy
/// issues, and the time it was made.
fn data_with_savings(name: &str) -> (PathBuf, i64) {
    let data = scratch(name);
    let c = store::open(&data).unwrap();
    store::create_project(&c, "shop").unwrap();
    store::create_project(&c, "api").unwrap();
    c.execute(
        "UPDATE projects SET log_retention_days=90 WHERE slug='shop'",
        [],
    )
    .unwrap();
    let now = time::now_ns();
    write_logs(&data, now);
    let now_ms = now / 1_000_000;
    let first = now_ms - 20 * DAY_MS;
    add(
        &c,
        first,
        &search(Some(1), 2 * DAY_NS, Some("payment"))(first),
    )
    .unwrap();
    let yesterday = now_ms - DAY_MS;
    add(
        &c,
        yesterday,
        &search(None, 6 * DAY_NS - 3_600_000_000_000, None)(yesterday),
    )
    .unwrap();
    add(
        &c,
        now_ms - 2 * HOUR_MS,
        &search(Some(2), 60 * DAY_NS, Some("health"))(now_ms),
    )
    .unwrap();
    let today = time::day_of(now);
    let back = |n: i64| today.checked_sub(n.days()).unwrap();
    log_day(&c, back(30), 10_000_000, false);
    log_day(&c, back(20), 5_000_000, true);
    log_day(&c, back(14), 1_000_000, false);
    log_day(&c, back(13), 7, false);
    for (n, message, count) in [
        (1, "payment declined", 700),
        (2, "timeout\u{1b}[2J", 300),
        (3, "disk full", 50),
    ] {
        let mut e = ErrorEvent::parse(serde_json::json!({
            "event_id": format!("{n:032x}"),
            "message": message,
        }))
        .unwrap();
        (e.received_at, e.timestamp) = (now_ms, now_ms);
        store::insert(&c, 1, &e).unwrap();
        c.execute(
            "UPDATE issue_hourly SET count=?2 WHERE issue_id=?1",
            (n, count),
        )
        .unwrap();
    }
    (data, now)
}

/// A report of the 7 days that end at `now`, with no forecast.
fn week(now: i64) -> Report {
    Report {
        since: time::day_of(now).checked_sub(6.days()).unwrap(),
        days: 7.0,
        disk: Err("not measured".into()),
        forecast: Err(NoForecast::Failed("not measured".into())),
        what_if: WhatIf::default(),
        projects: Vec::new(),
        default_log_days: None,
        now: 0,
        busy_no_project: 0,
    }
}

#[test]
fn the_savings_view_shows_patterns_retention_and_issues() {
    let (data, now) = data_with_savings("savings-view");
    let c = store::open(&data).unwrap();
    let report = week(now);
    let ask = |top| Request {
        project: Some("shop".into()),
        top,
        defaults: retention::Defaults::default(),
    };
    let found = savings(&c, &data, &report, &ask(2), now).unwrap();
    let d = sh(&data.display().to_string());
    let payment_at = time::rfc3339_ms(now / 1_000_000 - 20 * DAY_MS);
    let id = |m: &str| patterns::show(patterns::of(m).id);
    assert_eq!(
        found.json(false),
        json!({
            "window": {"since": report.since.to_string(), "days": 7.0},
            "savings": [
                {"kind": "pattern", "id": id("GET /health 200 12ms"), "project": "shop",
                 "service": "api", "level": "info", "example": "GET /health 200 12ms",
                 "bytes": 2_300, "per_day": 329, "share": 0.76, "last_queried": null,
                 "try": format!("search_logs pattern={}", id("GET /health 200 12ms")),
                 "suggest": format!("keep 1%; a person can run: davy drop add --pattern {} --keep 1% --data {d} -- shop", id("GET /health 200 12ms")),
                 "rule": format!("pattern {} below warn, keep 1%", id("GET /health 200 12ms")),
                 "command": format!("davy drop add --pattern {} --keep 1% --data {d} -- shop", id("GET /health 200 12ms")),
                 "saves_per_day": 325,
                 "shadow": format!(r#"drop_rule_shadow project="shop" pattern="{}" keep="1%""#, id("GET /health 200 12ms"))},
                {"kind": "pattern", "id": id("payment declined for order 42"), "project": "shop",
                 "service": "api", "level": "error", "example": "payment declined for order 42",
                 "bytes": 320, "per_day": 46, "share": 0.11, "last_queried": payment_at,
                 "try": format!("search_logs pattern={}", id("payment declined for order 42")),
                 "suggest": null, "rule": null, "command": null, "saves_per_day": null, "shadow": null},
                {"kind": "retention", "project": "shop", "logs_days": 90, "oldest_read_days": 6,
                 "searches": 2, "history_days": 19, "saves": 6_836_000,
                 "suggest": format!("keep 14 days; a person can run: davy project retention --logs 14 --data {d} -- shop"),
                 "command": format!("davy project retention --logs 14 --data {d} -- shop"), "try": r#"count_logs project="shop" since=90d until=14d"#},
                {"kind": "issue", "id": 1, "project": "shop", "title": "payment declined",
                 "events": 700, "share": 0.67,
                 "suggest": format!("fix it, or a person can discard it, which also stops storing its real events: davy issue discard 1 --data {d}"),
                 "command": format!("davy issue discard 1 --data {d}"), "try": "issue_context 1"},
                {"kind": "issue", "id": 2, "project": "shop", "title": "timeout\u{1b}[2J",
                 "events": 300, "share": 0.29,
                 "suggest": format!("fix it, or a person can discard it, which also stops storing its real events: davy issue discard 2 --data {d}"),
                 "command": format!("davy issue discard 2 --data {d}"), "try": "issue_context 2"},
            ],
            "note": NOTE,
        })
    );
    let human = found.json(true);
    assert_eq!(human["savings"][0]["bytes"], "2.3 KB");
    assert_eq!(human["savings"][2]["saves"], "6.8 MB");
    let mut out = Vec::new();
    found.text(&mut out).unwrap();
    let text = String::from_utf8(out).unwrap();
    let health = id("GET /health 200 12ms");
    assert!(
        text.contains(&format!(
            "pattern   {health} 2.3 KB (76%), 329 B/day, shop api info, last queried never\n"
        )),
        "{text}"
    );
    assert!(
        text.contains(&format!(
            "retention shop keeps 90 days of logs; 2 searches in 19 days, the oldest read 6 days back\n          \
             keep 14 days to save 6.8 MB\n          \
             a person can run: davy project retention --logs 14 --data {d} -- shop\n          \
             try: count_logs project=\"shop\" since=90d until=14d\n"
        )),
        "{text}"
    );
    assert!(text.contains("issue     2 in shop: 300 events, 29% of its error events\n          timeout\u{fffd}[2J\n"), "{text}");
    assert!(text.contains("          try: issue_context 2\n"), "{text}");
    assert!(!text.contains('\u{1b}'), "{text}");
    // 26 patterns, at most 20 shown.
    assert_eq!(
        savings(&c, &data, &report, &ask(50), now)
            .unwrap()
            .patterns
            .len(),
        MAX_TOP
    );
    std::fs::remove_dir_all(&data).unwrap();
}

#[test]
fn davys_own_project_gets_patterns_but_no_retention_or_issue_entry() {
    let (data, now) = data_with_savings("savings-own");
    let c = store::open(&data).unwrap();
    // Shop is Davy's own project here, with the searches, log days and
    // noisy issues that give it both entries above.
    c.execute("UPDATE projects SET own=1 WHERE slug='shop'", [])
        .unwrap();
    let report = week(now);
    for project in [Some("shop".to_owned()), None] {
        let ask = Request {
            project,
            top: 2,
            defaults: retention::Defaults::default(),
        };
        let found = savings(&c, &data, &report, &ask, now).unwrap();
        assert_eq!(found.patterns.len(), 2);
        assert_eq!((found.retention, found.issues), (Vec::new(), Vec::new()));
    }
    std::fs::remove_dir_all(&data).unwrap();
}

#[test]
fn a_retention_entry_puts_the_slug_last_and_says_one_day() {
    let fit = Fit {
        project: "-x".into(),
        logs_days: 30,
        oldest_read_days: 1,
        searches: 1,
        history_days: 7,
        suggest: 7,
        saves: 1_000,
    };
    let view = Savings {
        since: time::day_of(NOON_MS * 1_000_000),
        days: 7.0,
        patterns: Vec::new(),
        partial: false,
        shadow: Vec::new(),
        retention: vec![fit],
        issues: Vec::new(),
        data: "d".into(),
    };
    assert_eq!(
        view.json(false)["savings"][0]["suggest"],
        "keep 7 days; a person can run: davy project retention --logs 7 --data d -- -x"
    );
    assert_eq!(
        view.json(false)["savings"][0]["try"],
        r#"count_logs project="-x" since=30d until=7d"#
    );
    let mut out = Vec::new();
    view.text(&mut out).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(
        text.contains(
            "retention -x keeps 30 days of logs; 1 search in 7 days, the oldest read 1 day back\n"
        ),
        "{text}"
    );
}

#[path = "savings_drop_tests.rs"]
mod drop_tests;
