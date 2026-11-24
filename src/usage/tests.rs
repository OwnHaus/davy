use super::*;
use crate::{store, test_support::scratch};
use serde_json::json;

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

fn row(d: &str, project: i64, signal: Signal, records: u64, bytes: u64) -> Row {
    Row {
        day: day(d),
        project,
        signal,
        count: Count { records, bytes },
    }
}

fn stored(c: &Connection) -> Vec<(String, i64, String, i64, i64)> {
    let mut q = c
        .prepare("SELECT day, project_id, signal, records, bytes FROM usage_daily")
        .unwrap();
    q.query_map([], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
    })
    .unwrap()
    .map(Result::unwrap)
    .collect()
}

#[test]
fn writes_add_to_the_days_rows_and_skip_unknown_projects() {
    let c = memory();
    write(
        &c,
        &[
            row("2026-11-23", 1, Signal::Logs, 10, 1_000),
            row("2026-11-23", 9, Signal::Logs, 5, 50),
        ],
    )
    .unwrap();
    write(&c, &[row("2026-11-23", 1, Signal::Logs, 2, 300)]).unwrap();
    write(&c, &[row("2026-11-24", 2, Signal::Checkins, 1, 92)]).unwrap();
    assert_eq!(
        stored(&c),
        [
            ("2026-11-23".into(), 1, "logs".into(), 12, 1_300),
            ("2026-11-24".into(), 2, "checkins".into(), 1, 92),
        ]
    );
}

#[test]
fn the_sweep_keeps_400_days_of_counts() {
    let c = memory();
    write(
        &c,
        &[
            row("2025-10-19", 1, Signal::Errors, 1, 1),
            row("2025-10-20", 1, Signal::Errors, 2, 2),
        ],
    )
    .unwrap();
    // 2026-11-23 is 400 days after 2025-10-19.
    let at = jiff::Timestamp::from_second(1_795_392_000).unwrap();
    assert_eq!(time::day_of(time::nanos(at).unwrap()), day("2026-11-23"));
    prune(&c, at.as_millisecond()).unwrap();
    assert_eq!(
        stored(&c),
        [("2025-10-20".into(), 1, "errors".into(), 2, 2)]
    );
}

#[test]
fn counts_from_one_flush_that_found_the_queue_full_wait_for_the_next() {
    let counter = Counter::default();
    counter.put(day("2026-11-24"), 1, Signal::Logs, Count::new(3, 300));
    counter.add(1, Signal::Errors, Count::new(0, 0));
    let taken = counter.take();
    assert_eq!(taken, [row("2026-11-24", 1, Signal::Logs, 3, 300)]);
    counter.put(day("2026-11-24"), 1, Signal::Logs, Count::new(1, 10));
    counter.put_back(taken);
    assert_eq!(counter.take(), [row("2026-11-24", 1, Signal::Logs, 4, 310)]);
    assert_eq!(counter.take(), []);
}

#[tokio::test]
async fn the_flush_task_hands_the_counts_to_the_writer_and_stops_with_it() {
    let (sender, inbox) = crossbeam_channel::bounded(1);
    let counter = Counter::default();
    counter.put(day("2026-11-24"), 2, Signal::Feedback, Count::new(1, 60));
    let task = tokio::spawn(flush_every(
        counter.clone(),
        sender,
        Duration::from_millis(1),
    ));
    let write = loop {
        if let Ok(write) = inbox.try_recv() {
            break write;
        }
        tokio::task::yield_now().await;
    };
    let Job::Usage(rows) = write.job else {
        panic!("a usage job");
    };
    assert_eq!(rows, [row("2026-11-24", 2, Signal::Feedback, 1, 60)]);
    drop(inbox);
    counter.put(day("2026-11-24"), 2, Signal::Feedback, Count::new(1, 60));
    task.await.unwrap();
}

#[test]
fn the_migration_goes_down_and_up_again() {
    let mut c = memory();
    write(&c, &[row("2026-11-24", 1, Signal::Logs, 1, 1)]).unwrap();
    c.execute_batch(&format!("{UNDO} PRAGMA user_version=17;"))
        .unwrap();
    let tables = |c: &Connection| -> i64 {
        c.query_row(
            "SELECT count(*) FROM sqlite_master WHERE name='usage_daily'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(tables(&c), 0);
    store::init(&mut c).unwrap();
    assert_eq!(tables(&c), 1);
    assert_eq!(stored(&c), []);
    let version: i64 = c
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 18);
}

#[test]
fn the_disk_splits_into_log_days_sqlite_uploads_and_the_rest() {
    let data = scratch("usage-disk");
    let file = |path: &str, bytes: usize| {
        let path = data.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, vec![1_u8; bytes]).unwrap();
    };
    file("logs/2026-11-22/a", 100_000);
    file("logs/2026-11-23/a", 200_000);
    file("logs/2026-11-24/a", 50_000);
    file("logs/.trash/x", 10_000);
    file("davy.sqlite", 30_000);
    file("davy.sqlite-wal", 30_000);
    file("sourcemaps/b/map", 20_000);
    file("chunks/c", 20_000);
    file("setup.json", 1_000);
    let disk = disk::measure(&data, day("2026-11-24")).unwrap();
    let days: Vec<Date> = disk.closed.iter().map(|(d, _)| *d).collect();
    assert_eq!(days, [day("2026-11-22"), day("2026-11-23")]);
    // Allocated blocks round each file and directory up, so compare to
    // the bytes written with the rounding allowed.
    let near = |got: u64, want: u64| got >= want && got < want + 40_000;
    assert!(near(disk.closed[0].1, 100_000), "{disk:?}");
    assert!(near(disk.closed[1].1, 200_000), "{disk:?}");
    assert!(near(disk.today, 50_000), "{disk:?}");
    assert!(near(disk.sqlite, 60_000), "{disk:?}");
    assert!(near(disk.uploads, 40_000), "{disk:?}");
    assert!(near(disk.other, 11_000), "{disk:?}");
    assert!(disk.total > disk.free && disk.free > 0, "{disk:?}");
    std::fs::remove_dir_all(&data).unwrap();
}

#[test]
fn sizes_read_in_powers_of_1000() {
    let cases = [
        (0, "0 B"),
        (999, "999 B"),
        (1_000, "1.0 KB"),
        (4_149_999_999, "4.1 GB"),
        (4_150_000_000, "4.2 GB"),
        (u64::MAX, "18446.7 PB"),
    ];
    for (bytes, text) in cases {
        assert_eq!(report::human(bytes), text, "{bytes}");
    }
}

fn data_with_counts(name: &str) -> std::path::PathBuf {
    let data = scratch(name);
    let mut c = store::open(&data).unwrap();
    store::create_project(&c, "api").unwrap();
    store::create_project(&c, "shop").unwrap();
    c.execute(
        "UPDATE projects SET log_retention_days=7 WHERE slug='shop'",
        [],
    )
    .unwrap();
    let today = time::day_of(time::now_ns());
    let tx = c.transaction().unwrap();
    write(
        &tx,
        &[
            Row {
                day: today,
                project: 1,
                signal: Signal::Errors,
                count: Count::new(3, 3_000),
            },
            Row {
                day: today,
                project: 2,
                signal: Signal::Logs,
                count: Count::new(1_000, 2_500_000),
            },
            Row {
                day: today.checked_sub(30.days()).unwrap(),
                project: 2,
                signal: Signal::Logs,
                count: Count::new(9, 9),
            },
        ],
    )
    .unwrap();
    tx.commit().unwrap();
    data
}

#[test]
fn davy_usage_prints_the_counts_of_the_window_as_json() {
    let data = data_with_counts("usage-cli");
    let mut out = Vec::new();
    let args = Args {
        data: data.clone(),
        project: Some("shop".into()),
        since: "7d".into(),
        logs_days: Some(3),
        logs_bytes_per_day: None,
        log_retention_days: 30,
        json: true,
    };
    run(&mut out, &args).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(
        v["no_forecast"],
        "a forecast needs at least 2 closed log days, and the disk has 0"
    );
    assert_eq!(
        v["what_if"],
        json!({"project":"shop","logs_days":3,"logs_bytes_per_day":null})
    );
    assert_eq!(v["projects"].as_array().unwrap().len(), 1);
    let shop = &v["projects"][0];
    assert_eq!(
        (&shop["project"], &shop["logs_days"], &shop["errors_days"]),
        (&json!("shop"), &json!(7), &json!(36_500))
    );
    assert_eq!(shop["signals"]["logs"]["records"], 1_000);
    assert_eq!(shop["signals"]["logs"]["bytes"], 2_500_000);
    assert_eq!(shop["signals"]["errors"]["records"], 0);
    assert!(v["disk"]["total"].as_u64().unwrap() > 0);
    assert_eq!(v["disk"]["days_left"], json!(null));
    std::fs::remove_dir_all(&data).unwrap();
}

#[test]
fn davy_usage_prints_text_by_default_and_refuses_a_long_window() {
    let data = data_with_counts("usage-text");
    let mut args = Args {
        data: data.clone(),
        project: None,
        since: "7d".into(),
        logs_days: None,
        logs_bytes_per_day: None,
        log_retention_days: 30,
        json: false,
    };
    let mut out = Vec::new();
    run(&mut out, &args).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(
        text.contains(
            "forecast  no forecast: a forecast needs at least 2 closed log days, and the disk has 0\n"
        ),
        "{text}"
    );
    let api_errors = format!("{:<20} {:<9} {:>10} {:>10}", "api", "errors", 3, "3.0 KB");
    assert!(text.contains(&api_errors), "{text}");
    assert!(
        text.contains("shop                 keeps logs 7 days, errors 36500 days\n"),
        "{text}"
    );
    args.since = "91d".into();
    let err = run(&mut Vec::new(), &args).unwrap_err();
    assert_eq!(err.to_string(), "since reaches back at most 90d");
    std::fs::remove_dir_all(&data).unwrap();
}
