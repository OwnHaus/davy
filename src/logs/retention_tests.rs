use crate::logs::{
    LogLevel, LogRecord, LogSource, ORIGINAL_TIMESTAMP,
    store::{LogQuery, LogWriter, no_overrides, search},
};
use crate::test_support::scratch;
use crate::time::{day_of, now_ns};
use std::{collections::HashMap, path::Path};

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
    let overrides = Box::new(move || Ok(projects.clone()));
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
