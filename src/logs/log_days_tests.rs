use super::{LogDays, Mark, Row, Segment};
use crate::logs::{
    LogLevel, LogRecord, LogSource,
    store::{Overrides, State, no_overrides, schema},
};
use crate::retention::Lock;
use crate::test_support::scratch;
use crate::time::{day_of, now_ns};
use crate::usage::{Count, stored};
use jiff::civil::Date;
use rusqlite::Connection;
use std::{
    collections::{BTreeSet, HashMap},
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

const DAY: i64 = 86_400_000_000_000;
const PASS: Duration = Duration::from_secs(60);

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

fn row(day: Date, project: i64, records: u64, bytes: u64) -> Row {
    Row {
        day,
        project,
        count: Count { records, bytes },
    }
}

/// The counts `log_days` took since the last call, of every segment.
fn rows(log_days: &LogDays) -> Vec<Row> {
    let flush = log_days.take();
    flush.segments.into_iter().flat_map(|s| s.rows).collect()
}

/// A database with the projects api, shop and web, ids 1 to 3.
fn memory() -> Connection {
    let mut c = Connection::open_in_memory().unwrap();
    crate::store::init(&mut c).unwrap();
    for slug in ["api", "shop", "web"] {
        crate::store::create_project(&c, slug).unwrap();
    }
    c
}

/// `log_days` as `(day, project, records, cut)`, oldest first.
fn stored_rows(c: &Connection) -> Vec<(String, i64, i64, i64)> {
    let mut q = c
        .prepare("SELECT day, project_id, records, cut FROM log_days ORDER BY day, project_id")
        .unwrap();
    q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// A log writer's state whose projects keep what `settings` says.
fn with_settings(dir: &Path, default: u32) -> (State, Arc<Mutex<HashMap<u64, u32>>>) {
    let settings = Arc::new(Mutex::new(HashMap::new()));
    let read = {
        let settings = Arc::clone(&settings);
        move || Ok(settings.lock().unwrap().clone())
    };
    let state = State::new(
        dir.to_path_buf(),
        default,
        Overrides::new(read, Lock::default()),
    );
    (state, settings)
}

#[test]
fn log_days_counts_stored_raw_bytes_per_index_day() {
    let dir = scratch("log-days-count");
    let now = now_ns();
    let (today, yesterday) = (day_of(now), day_of(now - DAY));
    let mut state = State::new(dir.clone(), 30, no_overrides());
    let batch = vec![
        record(1, now, now, "a"),
        record(1, now, now, "bb"),
        record(2, now, now, "ccc"),
        record(1, now - DAY, now, "dddd"),
        record(2, now - DAY, now, "eeeee"),
    ];
    state.add(batch).unwrap();
    assert_eq!(rows(&state.log_days), [], "pending until committed");
    state.commit().unwrap();
    assert_eq!(
        rows(&state.log_days),
        [
            row(yesterday, 1, 1, 4),
            row(yesterday, 2, 1, 5),
            row(today, 1, 2, 3),
            row(today, 2, 1, 3),
        ]
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_failed_commit_takes_its_log_days_counts() {
    let dir = scratch("log-days-failed");
    let now = now_ns();
    let today = day_of(now);
    let mut state = State::new(dir.clone(), 30, no_overrides());
    state.add(vec![record(1, now, now, "kept")]).unwrap();
    state.commit().unwrap();
    state.add(vec![record(1, now, now, "lost!")]).unwrap();
    let day = dir.join(today.to_string());
    std::fs::rename(&day, dir.join("gone")).unwrap();
    assert!(state.close_all().is_err());
    // The next commit of the day settles its own count, not the lost one.
    state.add(vec![record(1, now, now, "again")]).unwrap();
    state.commit().unwrap();
    assert_eq!(rows(&state.log_days), [row(today, 1, 2, 9)]);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_backfilled_log_counts_on_its_index_day() {
    let dir = scratch("log-days-backfill");
    let now = now_ns();
    let mut state = State::new(dir.clone(), 30, no_overrides());
    let batch = vec![
        // Within retention: the index of its own day holds it.
        record(1, now - 3 * DAY, now, "late"),
        // 2000-01-01, past retention: today's index holds it, with its own
        // time as an attribute, which the bytes count.
        record(1, 946_684_800_000_000_000, now, "old"),
    ];
    state.add(batch).unwrap();
    state.commit().unwrap();
    let attribute = r#"{"davy.original_timestamp":946684800000000000}"#;
    assert_eq!(attribute.len(), 46);
    assert_eq!(
        rows(&state.log_days),
        [
            row(day_of(now - 3 * DAY), 1, 1, 4),
            row(day_of(now), 1, 1, 3 + 46)
        ]
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_day_index_without_the_pattern_field_still_counts() {
    let dir = scratch("log-days-no-pattern");
    let now = now_ns();
    let mut old = tantivy::schema::Schema::builder();
    for (_, entry) in schema().0.fields().filter(|(_, e)| e.name() != "pattern") {
        old.add_field(entry.clone());
    }
    let day = dir.join(day_of(now).to_string());
    std::fs::create_dir_all(&day).unwrap();
    tantivy::Index::create_in_dir(&day, old.build()).unwrap();
    let mut state = State::new(dir.clone(), 30, no_overrides());
    state
        .add(vec![record(1, now, now, "before patterns")])
        .unwrap();
    state.commit().unwrap();
    assert_eq!(rows(&state.log_days), [row(day_of(now), 1, 1, 15)]);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn log_days_outlive_log_retention_and_are_marked_cut() {
    let dir = scratch("log-days-cut");
    let now = now_ns();
    let old = now - 5 * DAY;
    let d = day_of(old);
    let c = memory();
    let (mut state, settings) = with_settings(&dir, 30);
    let batch = vec![
        record(1, old, old, "api"),
        record(2, old, old, "shop"),
        record(3, old, old, "web"),
    ];
    state.add(batch).unwrap();
    state.commit().unwrap();
    stored::write(&c, &state.log_days.take()).unwrap();
    settings.lock().unwrap().insert(1, 2);
    state.retention(now, PASS).unwrap();
    let flush = state.log_days.take();
    let marks = vec![Mark::Absent(BTreeSet::from([d])), Mark::Only(d, vec![1])];
    assert_eq!(
        flush.segments,
        [Segment {
            rows: Vec::new(),
            marks
        }]
    );
    stored::write(&c, &flush).unwrap();
    let day = d.to_string();
    assert_eq!(
        stored_rows(&c),
        [
            (day.clone(), 1, 1, 1),
            (day.clone(), 2, 1, 0),
            (day.clone(), 3, 1, 0)
        ]
    );
    state.close_all().unwrap();
    drop(state);
    // A server default of 2 days cuts web too; shop keeps its logs.
    let (mut state, settings) = with_settings(&dir, 2);
    *settings.lock().unwrap() = HashMap::from([(1, 2), (2, 30)]);
    state.retention(now, PASS).unwrap();
    let flush = state.log_days.take();
    assert_eq!(flush.segments[0].marks[1], Mark::AllBut(d, vec![2]));
    stored::write(&c, &flush).unwrap();
    let cut = |c: &Connection| -> Vec<i64> { stored_rows(c).iter().map(|r| r.3).collect() };
    assert_eq!(cut(&c), [1, 0, 1]);
    // A day removed whole marks every row.
    settings.lock().unwrap().clear();
    state.retention(now, PASS).unwrap();
    let flush = state.log_days.take();
    assert_eq!(flush.segments[0].marks, [Mark::AllBut(d, Vec::new())]);
    stored::write(&c, &flush).unwrap();
    assert_eq!(cut(&c), [1, 1, 1]);
    assert!(
        !dir.join(&day).exists(),
        "the logs are gone, the counts stay"
    );
    let records: Vec<i64> = stored_rows(&c).iter().map(|r| r.2).collect();
    assert_eq!(records, [1, 1, 1]);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_lost_cut_mark_is_restored_at_start() {
    let dir = scratch("log-days-repair");
    let now = now_ns();
    let (gone, cut) = (now - 6 * DAY, now - 5 * DAY);
    let c = memory();
    let (mut state, settings) = with_settings(&dir, 30);
    let batch = vec![
        record(1, gone, gone, "gone"),
        record(2, gone, gone, "gone too"),
        record(1, cut, cut, "api"),
        record(2, cut, cut, "shop"),
    ];
    state.add(batch).unwrap();
    state.commit().unwrap();
    stored::write(&c, &state.log_days.take()).unwrap();
    settings.lock().unwrap().insert(1, 2);
    state.retention(now, PASS).unwrap();
    state.close_all().unwrap();
    // A crash before the flush loses the marks; the oldest day goes by
    // hand meanwhile.
    drop(state.log_days.take());
    drop(state);
    std::fs::remove_dir_all(dir.join(day_of(gone).to_string())).unwrap();
    let (mut state, settings) = with_settings(&dir, 30);
    settings.lock().unwrap().insert(1, 2);
    state.retention(now, PASS).unwrap();
    stored::write(&c, &state.log_days.take()).unwrap();
    let (gone, cut) = (day_of(gone).to_string(), day_of(cut).to_string());
    assert_eq!(
        stored_rows(&c),
        [
            (gone.clone(), 1, 1, 1),
            (gone, 2, 1, 1),
            (cut.clone(), 1, 1, 1),
            (cut, 2, 1, 0)
        ]
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn counts_after_a_mark_are_not_marked_by_it() {
    let d: Date = "2026-11-20".parse().unwrap();
    let log_days = LogDays::default();
    log_days.add(d, HashMap::from([(1, Count::new(1, 10))]));
    log_days.mark(Mark::Absent(BTreeSet::new()));
    log_days.add(d, HashMap::from([(2, Count::new(1, 20))]));
    let c = memory();
    stored::write(&c, &log_days.take()).unwrap();
    let day = d.to_string();
    assert_eq!(stored_rows(&c), [(day.clone(), 1, 1, 1), (day, 2, 1, 0)]);
}

#[test]
fn a_job_the_writer_did_not_commit_goes_back_before_what_came_since() {
    let d: Date = "2026-11-20".parse().unwrap();
    let log_days = LogDays::new(1_795_392_000_000);
    let three = (1..=3).map(|p| (p, Count::new(1, 10))).collect();
    log_days.add(d, three);
    log_days.mark(Mark::Only(d, vec![1]));
    let first = log_days.take_up_to(2).unwrap();
    assert_eq!(first.start, Some(1_795_392_000_000));
    let segment = Segment {
        rows: vec![row(d, 1, 1, 10), row(d, 2, 1, 10)],
        marks: Vec::new(),
    };
    assert_eq!(first.segments, [segment]);
    log_days.add(d, HashMap::from([(1, Count::new(5, 50))]));
    log_days.put_back(first);
    let all = log_days.take();
    assert_eq!(all.start, Some(1_795_392_000_000));
    let rows: Vec<Vec<Row>> = all.segments.iter().map(|s| s.rows.clone()).collect();
    assert_eq!(
        rows,
        [
            vec![row(d, 1, 1, 10), row(d, 2, 1, 10)],
            vec![row(d, 3, 1, 10)],
            vec![row(d, 1, 5, 50)],
        ]
    );
    let marks: Vec<Vec<Mark>> = all.segments.into_iter().map(|s| s.marks).collect();
    assert_eq!(marks, [vec![], vec![Mark::Only(d, vec![1])], vec![]]);
    assert_eq!(log_days.take_up_to(500), None);
}

#[test]
fn a_start_that_finds_no_day_marks_nothing_until_one_comes_back() {
    let dir = scratch("log-days-empty-root");
    let now = now_ns();
    let (old, new) = (now - 2 * DAY, now);
    let c = memory();
    let (mut state, _) = with_settings(&dir, 30);
    state
        .add(vec![record(1, old, old, "old"), record(1, new, new, "new")])
        .unwrap();
    state.commit().unwrap();
    state.close_all().unwrap();
    stored::write(&c, &state.log_days.take()).unwrap();
    drop(state);
    // A restore that brought SQLite back first, or a mount that is missing.
    let away = dir.with_extension("away");
    std::fs::rename(&dir, &away).unwrap();
    std::fs::create_dir(&dir).unwrap();
    let (mut state, _) = with_settings(&dir, 30);
    state.retention(now, PASS).unwrap();
    stored::write(&c, &state.log_days.take()).unwrap();
    let (old, new) = (day_of(old).to_string(), day_of(new).to_string());
    assert_eq!(
        stored_rows(&c),
        [(old.clone(), 1, 1, 0), (new.clone(), 1, 1, 0)]
    );
    // The logs come back without the old day: that one start marks it.
    std::fs::remove_dir(&dir).unwrap();
    std::fs::rename(&away, &dir).unwrap();
    std::fs::remove_dir_all(dir.join(&old)).unwrap();
    let (mut state, _) = with_settings(&dir, 30);
    state.retention(now, PASS).unwrap();
    stored::write(&c, &state.log_days.take()).unwrap();
    assert_eq!(stored_rows(&c), [(old, 1, 1, 1), (new, 1, 1, 0)]);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_pass_that_deletes_nothing_more_does_not_mark_again() {
    let dir = scratch("log-days-mark-once");
    let now = now_ns();
    let old = now - 5 * DAY;
    let (mut state, settings) = with_settings(&dir, 30);
    let batch = vec![record(1, old, old, "api"), record(2, old, old, "shop")];
    state.add(batch).unwrap();
    state.commit().unwrap();
    settings.lock().unwrap().insert(1, 2);
    state.retention(now, PASS).unwrap();
    drop(state.log_days.take());
    // Each log of shop sends the next pass back to the day.
    for _ in 0..3 {
        state.add(vec![record(2, old, old, "shop")]).unwrap();
        state.commit().unwrap();
        state.retention(now, PASS).unwrap();
    }
    let flush = state.log_days.take();
    let marks: Vec<Mark> = flush.segments.into_iter().flat_map(|s| s.marks).collect();
    assert_eq!(marks, []);
    state.close_all().unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}
