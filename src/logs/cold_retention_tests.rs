use super::super::layout;
use crate::logs::{
    LogLevel, LogRecord, LogSource,
    store::log_days::Mark,
    store::{LogQuery, Overrides, State, search_all},
};
use crate::retention::Lock;
use crate::test_support::scratch;
use crate::time::{DAY_NS, day_of, midnight, now_ns};
use jiff::civil::Date;
use std::{
    collections::{BTreeSet, HashMap},
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

type Settings = Arc<Mutex<HashMap<u64, u32>>>;

fn record(project_id: i64, timestamp: i64) -> LogRecord {
    LogRecord {
        project_id,
        timestamp,
        observed_at: now_ns(),
        source: LogSource::Otlp,
        level: LogLevel::Info,
        level_unknown: false,
        message: format!("job of {project_id} done"),
        service: None,
        trace_id: None,
        span_id: None,
        attributes: serde_json::Map::new(),
    }
}

fn state(dir: &Path, default: u32, settings: &Settings) -> State {
    let read = Arc::clone(settings);
    let overrides = Overrides::new(move || Ok(read.lock().unwrap().clone()), Lock::default());
    State::new(dir.to_owned(), default, overrides)
}

/// Six logs of projects 1, 2 and 3 on the day `back` days ago, turned
/// cold.
fn cold_day(state: &mut State, back: i64) -> Date {
    let day = day_of(now_ns() - back * DAY_NS);
    let logs = (0..6).map(|i| record(1 + i % 3, midnight(day) + 1000 + i));
    state.add(logs.collect()).unwrap();
    state.commit().unwrap();
    state.turn_cold(day).unwrap();
    day
}

/// The projects of the logs a search finds, oldest first.
fn projects(dir: &Path) -> Vec<i64> {
    let q = LogQuery {
        project: None,
        since: 0,
        until: now_ns() + DAY_NS,
        min_level: None,
        text: None,
        pattern: None,
        limit: 100,
        skip: 0,
    };
    search_all(dir, &q)
        .unwrap()
        .iter()
        .map(|h| h.project_id)
        .collect()
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

fn marks(state: &State) -> Vec<Mark> {
    let flush = state.log_days.take();
    flush.segments.into_iter().flat_map(|s| s.marks).collect()
}

const PASS: Duration = Duration::from_secs(60);

#[test]
fn a_shorter_project_retention_takes_that_projects_cold_file() {
    let dir = scratch("cold-retention-project");
    let settings = Settings::default();
    let mut state = state(&dir, 30, &settings);
    let day = cold_day(&mut state, 10);
    // A late log makes the day hot and cold both.
    state
        .add(vec![
            record(2, midnight(day) + 9),
            record(1, midnight(day) + 8),
        ])
        .unwrap();
    state.commit().unwrap();
    state.retention(now_ns(), PASS).unwrap();
    drop(marks(&state));
    assert_eq!(projects(&dir), [1, 2, 1, 2, 3, 1, 2, 3]);

    settings.lock().unwrap().insert(2, 5);
    state.retention(now_ns(), PASS).unwrap();
    assert_eq!(projects(&dir), [1, 1, 3, 1, 3]);
    let seg = layout::segment_dir(&dir, day, 1);
    assert_eq!(
        names(&seg),
        ["1.dlc", "3.dlc", "patterns-1.json", "patterns-3.json"]
    );
    // The cold pass and the hot one mark the same pair.
    assert_eq!(
        marks(&state),
        [Mark::Only(day, vec![2]), Mark::Only(day, vec![2])]
    );
    state.retention(now_ns(), PASS).unwrap();
    assert_eq!(marks(&state), [], "once");

    settings.lock().unwrap().extend([(1, 5), (3, 5)]);
    state.retention(now_ns(), PASS).unwrap();
    assert_eq!(projects(&dir), Vec::<i64>::new());
    assert!(!layout::day_dir(&dir, day).exists());
    assert!(!dir.join(day.to_string()).exists());
    assert_eq!(names(&dir.join(".trash")), Vec::<String>::new());
    state.close_all().unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_cold_day_older_than_every_retention_goes_whole() {
    let dir = scratch("cold-retention-day");
    let settings = Settings::default();
    let mut state = state(&dir, 30, &settings);
    let old = cold_day(&mut state, 20);
    let young = cold_day(&mut state, 10);
    state.close_all().unwrap();
    // An empty directory a cut left, say, holds no logs of its day.
    let empty = day_of(now_ns() - 12 * DAY_NS);
    std::fs::create_dir(layout::day_dir(&dir, empty)).unwrap();

    let mut state = self::state(&dir, 15, &settings);
    state.retention(now_ns(), PASS).unwrap();
    assert!(!layout::day_dir(&dir, old).exists());
    assert_eq!(layout::segments(&dir, young).unwrap(), [1]);
    assert_eq!(projects(&dir), [1, 2, 3, 1, 2, 3]);
    let marks = marks(&state);
    assert_eq!(marks.len(), 2);
    assert_eq!(marks[0], Mark::Absent(BTreeSet::from([old, young])));
    assert_eq!(marks[1], Mark::AllBut(old, Vec::new()));
    assert_eq!(names(&dir.join(".trash")), Vec::<String>::new());
    state.close_all().unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_day_turning_cold_is_cut_once_it_is_cold() {
    let dir = scratch("cold-retention-busy");
    let settings = Settings::default();
    let mut state = state(&dir, 30, &settings);
    cold_day(&mut state, 10);
    let later = day_of(now_ns() - 9 * DAY_NS);
    state.add(vec![record(2, midnight(later) + 1)]).unwrap();
    state.commit().unwrap();
    state.start_job(later).unwrap();
    settings.lock().unwrap().insert(2, 5);
    // The hot pass stops the job, as it deletes from the day, and the cold
    // one leaves the day alone while it turns.
    state.retention(now_ns(), PASS).unwrap();
    assert_eq!(projects(&dir), [1, 3, 1, 3]);
    assert_eq!(layout::segments(&dir, later).unwrap(), Vec::<u32>::new());
    state.close_all().unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_hot_day_emptied_beside_its_segment_marks_only_what_the_cut_took() {
    let dir = scratch("cold-retention-hot-beside");
    let settings = Settings::default();
    let mut state = state(&dir, 30, &settings);
    let day = cold_day(&mut state, 10);
    // A late log of project 2 alone: the hot day empties with it.
    state.add(vec![record(2, midnight(day) + 9)]).unwrap();
    state.commit().unwrap();
    state.retention(now_ns(), PASS).unwrap();
    drop(marks(&state));

    settings.lock().unwrap().insert(2, 5);
    state.retention(now_ns(), PASS).unwrap();
    assert!(!dir.join(day.to_string()).exists());
    assert_eq!(projects(&dir), [1, 3, 1, 3]);
    assert_eq!(
        marks(&state),
        [Mark::Only(day, vec![2]), Mark::Only(day, vec![2])],
        "the logs of projects 1 and 3 are still there"
    );
    state.close_all().unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_cold_pass_leaves_a_day_alone_while_it_turns_cold_again() {
    let dir = scratch("cold-retention-turning");
    let settings = Settings::default();
    let mut state = state(&dir, 30, &settings);
    let day = cold_day(&mut state, 10);
    state.add(vec![record(2, midnight(day) + 9)]).unwrap();
    state.commit().unwrap();
    let (there, go) = state.cold.pause_next();
    state.start_job(day).unwrap();
    there.recv_timeout(PASS).unwrap();
    // A cold pass on its own, as when the one before the job failed or
    // the settings changed after the hot pass: cutting every project
    // empties segment 1, and the cold day would go whole with the segment
    // being written.
    settings.lock().unwrap().extend([(1, 5), (2, 5), (3, 5)]);
    state.refresh().unwrap();
    state.cold_retention(now_ns()).unwrap();
    assert_eq!(layout::segments(&dir, day).unwrap(), [1]);
    // Settings back as they were, so the hot day needs no cut and the
    // segment being written takes its place.
    let cut = std::mem::take(&mut *settings.lock().unwrap());
    go.send(()).unwrap();
    state.end_job().unwrap();
    assert_eq!(layout::segments(&dir, day).unwrap(), [1, 2]);
    assert!(!dir.join(day.to_string()).exists());

    *settings.lock().unwrap() = cut;
    state.retention(now_ns(), PASS).unwrap();
    assert_eq!(projects(&dir), Vec::<i64>::new());
    assert!(!layout::day_dir(&dir, day).exists());
    state.close_all().unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}
