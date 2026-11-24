use super::{PatternQuery, PatternReport, top_patterns};
use crate::logs::{
    LogLevel, LogRecord, LogSource,
    patterns::{self as masks, OTHER},
    store::{
        LogQuery, LogWriter, Overrides, State, histogram, no_overrides, schema, search,
        tally::{MAX_PATTERNS, file::name},
    },
};
use crate::test_support::scratch;
use crate::time::{day_of, now_ns};
use std::{collections::HashMap, path::Path};

const DAY: i64 = 86_400_000_000_000;

fn record(project_id: i64, timestamp: i64, level: LogLevel, message: &str) -> LogRecord {
    LogRecord {
        project_id,
        timestamp,
        observed_at: timestamp,
        source: LogSource::Logtail,
        level,
        message: message.into(),
        service: Some("api".into()),
        trace_id: None,
        span_id: None,
        attributes: serde_json::Map::new(),
    }
}

fn info(project_id: i64, timestamp: i64, message: &str) -> LogRecord {
    record(project_id, timestamp, LogLevel::Info, message)
}

/// Runs a log writer on `dir` until it has written `batch`, with
/// `projects` as the retention settings.
fn run(dir: &Path, projects: &[(u64, u32)], batch: Vec<LogRecord>) {
    let projects: HashMap<u64, u32> = projects.iter().copied().collect();
    let read = move || Ok(projects.clone());
    let overrides = Overrides::new(read, crate::retention::Lock::default());
    let writer = LogWriter::start(dir.to_path_buf(), 30, overrides).unwrap();
    if !batch.is_empty() {
        assert!(writer.queue.push(batch).is_ok());
    }
    drop(writer.queue);
    writer.thread.join().unwrap();
}

fn query(project: Option<i64>, now: i64) -> PatternQuery {
    PatternQuery {
        project,
        since: now - DAY,
        until: now + 1,
        min_level: None,
        limit: 10,
    }
}

fn report(dir: &Path, q: &PatternQuery) -> PatternReport {
    top_patterns(dir, q).unwrap()
}

fn rows(r: &PatternReport) -> Vec<(String, u64)> {
    r.top
        .iter()
        .map(|p| (p.template.clone(), p.records))
        .collect()
}

fn logs(pattern: u64, now: i64) -> LogQuery {
    LogQuery {
        project: None,
        since: now - DAY,
        until: now + 1,
        min_level: None,
        text: None,
        pattern: Some(pattern),
        limit: 100,
        skip: 0,
    }
}

fn batch(now: i64) -> Vec<LogRecord> {
    vec![
        info(1, now - 5, "GET /users/1 took 12ms"),
        info(1, now - 4, "GET /users/2 took 13ms"),
        info(1, now - 3, "GET /users/3 took 9ms"),
        record(1, now - 2, LogLevel::Error, "db timeout after 5000 ms"),
        info(2, now - 1, "GET /users/4 took 1ms"),
    ]
}

#[test]
fn the_top_patterns_come_with_template_example_volume_and_share() {
    let dir = scratch("patterns-top");
    let now = now_ns();
    run(&dir, &[], batch(now));
    let r = report(&dir, &query(Some(1), now));
    assert_eq!(
        rows(&r),
        [
            ("GET /users/<NUM> took <NUM>ms".into(), 3),
            ("db timeout after <NUM> ms".into(), 1)
        ]
    );
    assert_eq!((r.records, r.patterns, r.bytes.as_str()), (4, 2, "101 B"));
    let top = &r.top[0];
    assert_eq!(top.id, masks::show(masks::of("GET /users/9 took 1ms").id));
    assert_eq!(top.example, "GET /users/1 took 12ms");
    assert_eq!((top.bytes.as_str(), top.share.as_str()), ("74 B", "73.3%"));
    assert_eq!(top.levels.iter().collect::<Vec<_>>(), [(&"info", &3)]);
    assert_eq!(top.services, [(Some("api".into()), 3)]);
    assert_eq!(r.from, day_of(now - DAY).to_string());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_report_filters_by_level_and_sums_projects_when_none_is_named() {
    let dir = scratch("patterns-filter");
    let now = now_ns();
    run(&dir, &[], batch(now));
    let errors = PatternQuery {
        min_level: Some(LogLevel::Warn),
        ..query(None, now)
    };
    assert_eq!(
        rows(&report(&dir, &errors)),
        [("db timeout after <NUM> ms".into(), 1)]
    );
    let all = report(&dir, &query(None, now));
    assert_eq!(rows(&all)[0], ("GET /users/<NUM> took <NUM>ms".into(), 4));
    let one = PatternQuery {
        limit: 1,
        ..query(None, now)
    };
    let limited = report(&dir, &one);
    assert_eq!((limited.top.len(), limited.patterns), (1, 2));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn counters_survive_a_restart() {
    let dir = scratch("patterns-restart");
    let now = now_ns();
    run(&dir, &[], batch(now));
    assert!(dir.join(day_of(now).to_string()).join(name(1)).exists());
    run(&dir, &[], vec![info(1, now, "GET /users/5 took 2ms")]);
    assert_eq!(rows(&report(&dir, &query(Some(1), now)))[0].1, 4);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_pattern_finds_its_lines_and_counts_as_many_as_the_report() {
    let dir = scratch("patterns-search");
    let now = now_ns();
    run(&dir, &[], batch(now));
    let id = masks::of("GET /users/1 took 12ms").id;
    let hits = search(&dir, &logs(id, now)).unwrap();
    assert_eq!(hits.len(), 4);
    assert!(hits.iter().all(|h| h.pattern == Some(masks::show(id))));
    let counts = histogram(&dir, &logs(id, now)).unwrap();
    assert_eq!(counts.counts.iter().sum::<u64>(), 4);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn lines_over_the_bound_are_found_as_other() {
    let dir = scratch("patterns-other");
    let now = now_ns();
    let lines: Vec<LogRecord> = (0..MAX_PATTERNS + 3)
        .map(|n| {
            let word: String = format!("{n:04}")
                .bytes()
                .map(|d| char::from(b'a' + d - b'0'))
                .collect();
            info(1, now - 1, &format!("event {word}"))
        })
        .collect();
    run(&dir, &[], lines);
    let other = search(&dir, &logs(OTHER, now)).unwrap();
    assert_eq!(other.len(), 3);
    assert!(other.iter().all(|h| h.pattern.as_deref() == Some("other")));
    let r = report(&dir, &query(Some(1), now));
    let row = r.top.iter().find(|p| p.id == "other").unwrap();
    assert_eq!(row.records, 3);
    assert_eq!(r.patterns, MAX_PATTERNS + 1);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn retention_takes_the_counters_with_the_logs() {
    let dir = scratch("patterns-retention");
    let now = now_ns();
    let old = now - 5 * DAY;
    run(
        &dir,
        &[],
        vec![info(1, old, "kept 1"), info(2, old, "cut 1")],
    );
    run(&dir, &[(2, 3)], vec![]);
    let q = PatternQuery {
        since: old,
        ..query(None, now)
    };
    assert_eq!(rows(&report(&dir, &q)), [("kept <NUM>".into(), 1)]);
    run(&dir, &[(1, 3), (2, 3)], vec![]);
    let gone = report(&dir, &q);
    assert_eq!((gone.records, gone.top.len()), (0, 0));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_report_sees_counts_the_writer_has_not_saved_yet() {
    let dir = scratch("patterns-live");
    let now = now_ns();
    let mut state = State::new(dir.clone(), 30, no_overrides());
    state.add(batch(now)).unwrap();
    assert_eq!(report(&dir, &query(Some(1), now)).records, 0);
    state.commit().unwrap();
    assert!(!dir.join(day_of(now).to_string()).join(name(1)).exists());
    assert_eq!(report(&dir, &query(Some(1), now)).records, 4);
    state.close_all().unwrap();
    drop(state);
    assert_eq!(report(&dir, &query(Some(1), now)).records, 4);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn logs_lost_in_a_failed_commit_are_not_counted() {
    let dir = scratch("patterns-lost");
    let now = now_ns();
    let day = dir.join(day_of(now).to_string());
    let mut state = State::new(dir.clone(), 30, no_overrides());
    state.add(batch(now)).unwrap();
    // A rename takes the directory away at once, whatever the indexing
    // threads still write into it.
    std::fs::rename(&day, dir.join("gone")).unwrap();
    assert!(state.commit().is_err());
    std::fs::create_dir(&day).unwrap();
    assert_eq!(report(&dir, &query(Some(1), now)).records, 0);
    state
        .add(vec![info(1, now, "GET /users/5 took 2ms")])
        .unwrap();
    state.commit().unwrap();
    let r = report(&dir, &query(Some(1), now));
    assert_eq!(rows(&r), [("GET /users/<NUM> took <NUM>ms".into(), 1)]);
    assert_eq!(r.top[0].example, "GET /users/5 took 2ms");
    state.close_all().unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_day_written_before_patterns_counts_but_finds_nothing_by_pattern() {
    let dir = scratch("patterns-old-day");
    let now = now_ns();
    let mut old = tantivy::schema::Schema::builder();
    for (_, entry) in schema().0.fields().filter(|(_, e)| e.name() != "pattern") {
        old.add_field(entry.clone());
    }
    let day = dir.join(day_of(now).to_string());
    std::fs::create_dir_all(&day).unwrap();
    tantivy::Index::create_in_dir(&day, old.build()).unwrap();
    run(&dir, &[], batch(now));
    let id = masks::of("GET /users/1 took 12ms").id;
    assert_eq!(search(&dir, &logs(id, now)).unwrap().len(), 0);
    let all = LogQuery {
        pattern: None,
        ..logs(id, now)
    };
    let hits = search(&dir, &all).unwrap();
    assert_eq!(hits.len(), 5);
    assert!(hits.iter().all(|h| h.pattern.is_none()));
    assert_eq!(report(&dir, &query(Some(1), now)).records, 4);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn an_unreadable_file_is_left_out_named_and_set_aside_by_the_writer() {
    let dir = scratch("patterns-unread");
    let now = now_ns();
    run(&dir, &[], batch(now));
    let day = dir.join(day_of(now).to_string());
    std::fs::write(day.join(name(1)), "{").unwrap();
    let r = report(&dir, &query(None, now));
    assert_eq!(
        (r.records, r.unread.clone()),
        (1, vec![day_of(now).to_string()])
    );
    run(&dir, &[], vec![info(1, now, "GET /users/5 took 2ms")]);
    let r = report(&dir, &query(Some(1), now));
    assert_eq!((r.records, r.unread.len()), (1, 0));
    let aside: Vec<String> = std::fs::read_dir(&day)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.starts_with(&format!("{}.unread-", name(1))))
        .collect();
    assert_eq!(aside.len(), 1);
    assert_eq!(std::fs::read_to_string(day.join(&aside[0])).unwrap(), "{");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn counts_at_the_top_of_u64_add_up_to_the_top_and_no_further() {
    let dir = scratch("patterns-saturate");
    let now = now_ns();
    let full = [0, 0, u64::MAX, 0, 0, 0].map(|n| [n, n]);
    let entry = serde_json::json!({"template": "a", "example": "a", "services": {"": full}});
    let text = serde_json::json!({"version": 1, "patterns": {"5": entry}}).to_string();
    for day in [day_of(now), day_of(now - DAY)] {
        let day = dir.join(day.to_string());
        std::fs::create_dir_all(&day).unwrap();
        std::fs::write(day.join(name(1)), &text).unwrap();
    }
    let r = report(&dir, &query(Some(1), now));
    assert_eq!((r.records, r.top[0].records), (u64::MAX, u64::MAX));
    assert_eq!(
        r.top[0].levels.iter().collect::<Vec<_>>(),
        [(&"info", &u64::MAX)]
    );
    std::fs::remove_dir_all(&dir).unwrap();
}
