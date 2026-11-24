use super::stray_line;
use crate::logs::{
    LogLevel, LogRecord, LogSource, ORIGINAL_TIMESTAMP,
    store::{LogQuery, LogWriter, Overrides, State, Stray, no_overrides, search, strays},
};
use crate::retention::Lock;
use crate::test_support::scratch;
use crate::time::{day_of, now_ns};
use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Mutex},
};

const DAY: i64 = 86_400_000_000_000;

fn record(project_id: i64, timestamp: i64, observed_at: i64, message: &str) -> LogRecord {
    LogRecord {
        project_id,
        timestamp,
        observed_at,
        source: LogSource::Logtail,
        level: LogLevel::Info,
        message: message.into(),
        service: None,
        trace_id: None,
        span_id: None,
        attributes: serde_json::Map::new(),
    }
}

/// Runs a log writer on `dir` until it has written `batch`: one retention
/// pass with `projects` as the settings, then the batch.
fn run(dir: &Path, default: u32, projects: &[(u64, u32)], batch: Vec<LogRecord>) {
    let projects: HashMap<u64, u32> = projects.iter().copied().collect();
    let read = move || Ok(projects.clone());
    run_with(dir, default, Overrides::new(read, Lock::default()), batch);
}

/// [`run`] with the settings `overrides` reads.
fn run_with(dir: &Path, default: u32, overrides: Overrides, batch: Vec<LogRecord>) {
    let writer = LogWriter::start(dir.to_path_buf(), default, overrides).unwrap();
    if !batch.is_empty() {
        assert!(writer.queue.push(batch).is_ok());
    }
    drop(writer.queue);
    writer.thread.join().unwrap();
}

/// The messages kept, oldest first.
fn messages(dir: &Path) -> Vec<String> {
    let q = LogQuery {
        project: None,
        since: 0,
        until: now_ns() + DAY,
        min_level: None,
        text: None,
        limit: 100,
        skip: 0,
    };
    let hits = search(dir, &q).unwrap();
    hits.into_iter().map(|h| h.message).collect()
}

fn day_dirs(dir: &Path) -> Vec<String> {
    let mut days: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|name| !name.starts_with('.'))
        .collect();
    days.sort();
    days
}

/// Whether a segment of the day of `at` still holds deleted logs.
fn holds_deleted(dir: &Path, at: i64) -> bool {
    let index = tantivy::Index::open_in_dir(dir.join(day_of(at).to_string())).unwrap();
    let metas = index.searchable_segment_metas().unwrap();
    metas.iter().any(tantivy::index::SegmentMeta::has_deletes)
}

#[test]
fn a_shorter_project_retention_deletes_only_that_projects_old_logs() {
    let dir = scratch("logs-retention-short");
    let now = now_ns();
    let old = now - 5 * DAY;
    let batch = vec![
        record(1, old, old, "api old"),
        record(2, old + 1, old + 1, "web old"),
        record(1, now, now, "api new"),
    ];
    run(&dir, 30, &[], batch);
    assert_eq!(messages(&dir), ["api old", "web old", "api new"]);
    run(&dir, 30, &[(1, 2)], vec![]);
    assert_eq!(messages(&dir), ["web old", "api new"]);
    // Merged away, not only hidden from searches.
    assert!(!holds_deleted(&dir, old));
    // A day that loses its last logs goes whole.
    run(&dir, 30, &[(1, 2), (2, 3)], vec![]);
    assert_eq!(messages(&dir), ["api new"]);
    assert_eq!(day_dirs(&dir), [day_of(now).to_string()]);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_broken_day_is_skipped_and_the_trash_is_emptied() {
    let dir = scratch("logs-retention-broken");
    let now = now_ns();
    let old = now - 5 * DAY;
    let batch = vec![
        record(1, old, old, "api old"),
        record(2, old + 1, old + 1, "web old"),
    ];
    run(&dir, 30, &[], batch);
    // An older day that no index opens, and a day a crash left in the trash.
    let broken = dir.join(day_of(old - DAY).to_string());
    std::fs::create_dir(&broken).unwrap();
    let leftover = dir.join(".trash").join("2020-01-01-1");
    std::fs::create_dir_all(&leftover).unwrap();
    std::fs::write(leftover.join("segment"), "x").unwrap();
    run(&dir, 30, &[(1, 2)], vec![]);
    assert!(broken.is_dir());
    assert_eq!(std::fs::read_dir(dir.join(".trash")).unwrap().count(), 0);
    std::fs::remove_dir(&broken).unwrap();
    assert_eq!(messages(&dir), ["web old"]);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_pass_that_cannot_read_the_settings_deletes_nothing() {
    let dir = scratch("logs-retention-unread");
    let now = now_ns();
    let old = now - 5 * DAY;
    let batch = vec![
        record(1, old, old, "api old"),
        record(2, old + 1, old + 1, "web old"),
    ];
    run(&dir, 2, &[(1, 30)], batch);
    // Read as no settings, the default of 2 days would delete the whole day,
    // the logs of project 1, which keeps 30, included.
    let unread = || Err(anyhow::anyhow!("database is locked"));
    run_with(&dir, 2, Overrides::new(unread, Lock::default()), vec![]);
    assert_eq!(messages(&dir), ["api old", "web old"]);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_pass_whose_data_directory_another_process_holds_deletes_nothing_and_writes() {
    let dir = scratch("logs-retention-held");
    let old = now_ns() - 5 * DAY;
    run(&dir, 30, &[], vec![record(1, old, old, "api old")]);
    // A CLI that stopped while it held the lock on the data directory.
    let data = scratch("logs-retention-held-data");
    std::fs::create_dir_all(&data).unwrap();
    let cli = Lock::default()
        .with_dir(&data)
        .waiting(crate::retention::CLI_WAIT);
    let held = cli.hold_all().unwrap();
    let lock = Lock::default()
        .with_dir(&data)
        .waiting(std::time::Duration::from_millis(50));
    let (root, (ran, done)) = (dir.clone(), std::sync::mpsc::channel());
    std::thread::spawn(move || {
        let read = || Ok(HashMap::new());
        let batch = vec![record(1, now_ns(), now_ns(), "api new")];
        run_with(&root, 2, Overrides::new(read, lock), batch);
        ran.send(()).unwrap();
    });
    assert_eq!(
        done.recv_timeout(std::time::Duration::from_secs(10)),
        Ok(())
    );
    assert_eq!(messages(&dir), ["api old", "api new"]);
    drop(held);
    std::fs::remove_dir_all(&dir).unwrap();
    std::fs::remove_dir_all(&data).unwrap();
}

#[test]
fn a_change_saved_during_a_pass_is_read_before_the_day_is_deleted_from() {
    let dir = scratch("logs-retention-changed");
    let now = now_ns();
    let old = now - 5 * DAY;
    let batch = vec![
        record(1, old, old, "api old"),
        record(2, old + 1, old + 1, "web old"),
    ];
    run(&dir, 30, &[], batch);
    // Project 1 keeps 2 days when the pass starts, and 30 once a change
    // is saved. Each read says whether the lock within the server and the
    // lock on the data directory the CLI takes were held during it.
    let data = scratch("logs-retention-changed-data");
    std::fs::create_dir_all(&data).unwrap();
    let lock = Lock::default().with_dir(&data);
    let held = Arc::new(Mutex::new(Vec::new()));
    let (seen, reads, root) = (lock.clone(), Arc::clone(&held), data.clone());
    let read = move || {
        let mut reads = reads.lock().unwrap();
        let locked = std::fs::File::open(&root).is_ok_and(|f| f.try_lock().is_err());
        reads.push((seen.is_held(), locked));
        let days = if reads.len() == 1 { 2 } else { 30 };
        Ok(HashMap::from([(1, days)]))
    };
    run_with(&dir, 30, Overrides::new(read, lock), vec![]);
    assert_eq!(messages(&dir), ["api old", "web old"]);
    assert_eq!(*held.lock().unwrap(), [(false, false), (true, true)]);
    std::fs::remove_dir_all(&dir).unwrap();
    std::fs::remove_dir_all(&data).unwrap();
}

#[test]
fn a_failed_read_before_a_day_stops_the_pass_there() {
    let dir = scratch("logs-retention-failed-read");
    let now = now_ns();
    let (first, second) = (now - 5 * DAY, now - 4 * DAY);
    let batch = vec![
        record(1, first, first, "api first"),
        record(2, first + 1, first + 1, "web first"),
        record(1, second, second, "api second"),
        record(2, second + 1, second + 1, "web second"),
    ];
    run(&dir, 30, &[], batch);
    // Project 1 keeps 2 days. The reads at the start of the pass and before
    // the first day work, the one before the second day fails.
    let count = Arc::new(Mutex::new(0));
    let reads = Arc::clone(&count);
    let read = move || {
        let mut reads = reads.lock().unwrap();
        *reads += 1;
        if *reads == 3 {
            anyhow::bail!("database is locked");
        }
        Ok(HashMap::from([(1, 2)]))
    };
    // No time budget, so only the failed read can end the pass early.
    let mut state = State::new(dir.clone(), 30, Overrides::new(read, Lock::default()));
    let pass = state.retention(now, std::time::Duration::MAX);
    assert_eq!(
        pass.map_err(|e| format!("{e:#}")),
        Err("reading the retention settings: database is locked".into())
    );
    drop(state);
    assert_eq!(messages(&dir), ["web first", "api second", "web second"]);
    assert_eq!(*count.lock().unwrap(), 3);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_whole_day_is_deleted_under_the_lock() {
    let dir = scratch("logs-retention-whole-day");
    let old = now_ns() - 5 * DAY;
    run(&dir, 30, &[], vec![record(1, old, old, "api old")]);
    let lock = Lock::default();
    let held = Arc::new(Mutex::new(Vec::new()));
    let (seen, reads) = (lock.clone(), Arc::clone(&held));
    let read = move || {
        reads.lock().unwrap().push(seen.is_held());
        Ok(HashMap::new())
    };
    run_with(&dir, 2, Overrides::new(read, lock), vec![]);
    assert_eq!(day_dirs(&dir), Vec::<String>::new());
    assert_eq!(*held.lock().unwrap(), [false, true]);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_log_written_to_a_day_after_its_pass_is_deleted_by_the_next() {
    let dir = scratch("logs-retention-late");
    let now = now_ns();
    let old = now - 5 * DAY;
    let projects = HashMap::from([(1, 2)]);
    let overrides = Overrides::new(move || Ok(projects.clone()), Lock::default());
    let mut state = State::new(dir.clone(), 30, overrides);
    state.add(vec![record(2, old, old, "web old")]).unwrap();
    state.commit().unwrap();
    // The day holds nothing of project 1, which keeps 2 days.
    let budget = std::time::Duration::from_secs(60);
    state.retention(now, budget).unwrap();
    // A log observed when its time was within those 2 days reaches the
    // writer after the pass.
    let late = record(1, old + 1, old + 1 + DAY, "api late");
    state.add(vec![late]).unwrap();
    state.retention(now, budget).unwrap();
    state.close_all().unwrap();
    assert_eq!(messages(&dir), ["web old"]);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_symlink_named_like_a_day_is_not_searched_written_or_deleted_from() {
    let dir = scratch("logs-retention-link");
    let elsewhere = scratch("logs-retention-link-target");
    let now = now_ns();
    let old = now - 5 * DAY;
    run(
        &elsewhere,
        30,
        &[],
        vec![record(1, old, old, "api elsewhere")],
    );
    let day = day_of(old).to_string();
    std::fs::create_dir_all(&dir).unwrap();
    std::os::unix::fs::symlink(elsewhere.join(&day), dir.join(&day)).unwrap();
    // Not the canonical name of a day either.
    std::fs::create_dir(dir.join("+002026-01-05")).unwrap();
    run(&dir, 30, &[(1, 2)], vec![]);
    assert_eq!(messages(&elsewhere), ["api elsewhere"]);
    assert!(!holds_deleted(&elsewhere, old));
    assert!(dir.join(&day).is_symlink());
    // A log of that day fails to write rather than land where the link
    // points, and a search reads nothing through it.
    run(&dir, 30, &[], vec![record(1, old + 1, old + 1, "api here")]);
    assert_eq!(messages(&elsewhere), ["api elsewhere"]);
    assert_eq!(messages(&dir), Vec::<String>::new());
    let strays: Vec<(String, &str)> = strays(&dir)
        .unwrap()
        .into_iter()
        .map(|s| (s.name, s.why))
        .collect();
    assert_eq!(
        strays,
        [
            (
                "+002026-01-05".to_owned(),
                "names a date, but a day's directory is named YYYY-MM-DD"
            ),
            (day.clone(), "is a symlink, not the day's directory"),
        ]
    );
    // Retention reports each stray once, not every pass.
    let mut state = State::new(dir.clone(), 30, no_overrides());
    for _ in 0..2 {
        state.retention(now, std::time::Duration::MAX).unwrap();
    }
    let mut reported: Vec<&String> = state.strays.iter().collect();
    reported.sort();
    assert_eq!(reported, ["+002026-01-05", &day]);
    std::fs::remove_dir_all(&dir).unwrap();
    std::fs::remove_dir_all(&elsewhere).unwrap();
}

#[test]
fn a_stray_that_is_moved_away_and_comes_back_is_reported_again() {
    let dir = scratch("logs-retention-stray-back");
    let stray = dir.join("+002026-01-05");
    std::fs::create_dir_all(&stray).unwrap();
    let mut state = State::new(dir.clone(), 30, no_overrides());
    state.retention(now_ns(), std::time::Duration::MAX).unwrap();
    assert_eq!(state.strays.len(), 1);
    std::fs::remove_dir(&stray).unwrap();
    state.retention(now_ns(), std::time::Duration::MAX).unwrap();
    assert_eq!(state.strays.len(), 0);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_stray_is_reported_with_its_path_quoted_and_escaped() {
    let stray = Stray {
        name: "2026-01-05\nlog writer: forged".into(),
        why: "is not a directory",
    };
    assert_eq!(
        stray_line(Path::new("/logs"), &stray),
        r#"log writer: "/logs/2026-01-05\nlog writer: forged" is not a directory, so it is not searched, written or deleted until it is moved away"#
    );
}

#[test]
fn a_stray_in_a_days_place_loses_only_that_days_logs_of_a_batch() {
    let dir = scratch("logs-retention-stray-batch");
    let (now, old) = (now_ns(), now_ns() - 5 * DAY);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(day_of(old).to_string()), "not a day").unwrap();
    let batch = vec![
        record(1, old, old, "api stray"),
        record(1, now, now, "api today"),
    ];
    run(&dir, 30, &[], batch);
    assert_eq!(messages(&dir), ["api today"]);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_directory_of_a_year_with_six_digits_is_not_a_day() {
    let dir = scratch("logs-retention-long-year");
    // jiff reads and prints the year -1 so, and that day is long expired.
    let year = dir.join("-000001-01-01");
    std::fs::create_dir_all(&year).unwrap();
    std::fs::write(year.join("notes"), "kept").unwrap();
    run(&dir, 30, &[], vec![]);
    assert_eq!(std::fs::read_to_string(year.join("notes")).unwrap(), "kept");
    let names: Vec<String> = strays(&dir).unwrap().into_iter().map(|s| s.name).collect();
    assert_eq!(names, ["-000001-01-01"]);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_trash_is_emptied_only_of_what_retention_put_there() {
    let dir = scratch("logs-retention-trash");
    let elsewhere = scratch("logs-retention-trash-target");
    std::fs::create_dir_all(elsewhere.join("2020-01-01-1")).unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    std::os::unix::fs::symlink(&elsewhere, dir.join(".trash")).unwrap();
    run(&dir, 30, &[], vec![]);
    assert!(elsewhere.join("2020-01-01-1").is_dir());
    std::fs::remove_file(dir.join(".trash")).unwrap();
    let trash = dir.join(".trash");
    for name in ["2020-01-01-1", "notes", "2020-01-01-", "2020-01-01-1x"] {
        std::fs::create_dir_all(trash.join(name)).unwrap();
    }
    std::os::unix::fs::symlink(&elsewhere, trash.join("2020-01-01-2")).unwrap();
    run(&dir, 30, &[], vec![]);
    let mut left: Vec<String> = std::fs::read_dir(&trash)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    left.sort();
    assert_eq!(
        left,
        ["2020-01-01-", "2020-01-01-1x", "2020-01-01-2", "notes"]
    );
    assert!(elsewhere.join("2020-01-01-1").is_dir());
    std::fs::remove_dir_all(&dir).unwrap();
    std::fs::remove_dir_all(&elsewhere).unwrap();
}

#[test]
fn a_longer_project_retention_keeps_a_day_the_default_would_delete() {
    let dir = scratch("logs-retention-long");
    let now = now_ns();
    let old = now - 5 * DAY;
    let batch = vec![
        record(1, old, old, "api old"),
        record(2, old + 1, old + 1, "web old"),
    ];
    // Written after the pass of this run, so the next run's pass sees them.
    run(&dir, 2, &[(1, 30)], batch);
    assert_eq!(messages(&dir), ["api old", "web old"]);
    run(&dir, 2, &[(1, 30)], vec![]);
    assert_eq!(messages(&dir), ["api old"]);
    assert_eq!(day_dirs(&dir), [day_of(old).to_string()]);
    run(&dir, 2, &[], vec![]);
    assert_eq!(messages(&dir), Vec::<String>::new());
    assert_eq!(day_dirs(&dir), Vec::<String>::new());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_late_log_is_dated_by_its_own_projects_retention() {
    let dir = scratch("logs-retention-settle");
    let now = now_ns();
    let late = now - 10 * DAY;
    let batch = vec![
        record(1, late, now, "api late"),
        record(2, late, now, "web late"),
    ];
    run(&dir, 2, &[(1, 30)], batch);
    let q = LogQuery {
        project: None,
        since: 0,
        until: now + DAY,
        min_level: None,
        text: None,
        limit: 10,
        skip: 0,
    };
    let hits = search(&dir, &q).unwrap();
    let dated: Vec<(&str, i64)> = hits
        .iter()
        .map(|h| (h.message.as_str(), h.timestamp_ns))
        .collect();
    assert_eq!(dated, [("api late", late), ("web late", now)]);
    assert_eq!(hits[1].attributes[ORIGINAL_TIMESTAMP], late);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_server_default_is_1_to_36500_days() {
    let dir = scratch("logs-default");
    for days in [0, 36_501, u32::MAX] {
        let refused = LogWriter::start(dir.clone(), days, no_overrides());
        assert!(refused.is_err(), "{days}");
    }
    let writer = LogWriter::start(dir, 36_500, no_overrides()).unwrap();
    drop(writer.queue);
    writer.thread.join().unwrap();
}

#[test]
fn settings_that_cannot_be_read_are_read_again_within_seconds() {
    let dir = scratch("logs-retention-retry");
    let (sent, reads) = std::sync::mpsc::channel();
    let mut count = 0;
    let read = move || {
        count += 1;
        sent.send(count).unwrap();
        if count == 1 {
            Err(anyhow::anyhow!("database is locked"))
        } else {
            Ok(HashMap::new())
        }
    };
    let writer = LogWriter::start(dir.clone(), 30, Overrides::new(read, Lock::default())).unwrap();
    // Until the second read, a log is timed by the server default. The
    // next pass after one that worked is a minute away.
    let wait = std::time::Duration::from_secs(15);
    let seen: Vec<_> = (0..2).map(|_| reads.recv_timeout(wait).ok()).collect();
    assert_eq!(seen, [Some(1), Some(2)]);
    drop(writer.queue);
    writer.thread.join().unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}
