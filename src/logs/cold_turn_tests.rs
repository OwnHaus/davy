use super::{super::layout, Step};
use crate::logs::{
    LogLevel, LogRecord, LogSource,
    store::{
        LogHit, LogQuery, PatternQuery, PatternReport, State, histogram_all, no_overrides,
        search_all, top_patterns,
    },
};
use crate::test_support::scratch;
use crate::time::{DAY_NS, day_of, midnight, now_ns};
use jiff::civil::Date;
use std::path::Path;

pub(super) fn record(project_id: i64, timestamp: i64, message: &str) -> LogRecord {
    let mut attributes = serde_json::Map::new();
    attributes.insert("n".into(), timestamp.rem_euclid(7).into());
    attributes.insert("user".into(), format!("u{}", timestamp % 3).into());
    LogRecord {
        project_id,
        timestamp,
        observed_at: now_ns(),
        source: LogSource::Otlp,
        level: if timestamp % 4 == 0 {
            LogLevel::Error
        } else {
            LogLevel::Info
        },
        level_unknown: false,
        message: message.into(),
        service: Some("api".into()),
        trace_id: None,
        span_id: None,
        attributes,
    }
}

/// A day ten days back, and a state with 30 logs of two projects in it.
pub(super) fn ten_days_back(name: &str) -> (std::path::PathBuf, State, Date) {
    let dir = scratch(name);
    let day = day_of(now_ns() - 10 * DAY_NS);
    let mut state = State::new(dir.clone(), 30, no_overrides());
    let start = midnight(day) + 3_600_000_000_000;
    let batch = (0..30)
        .map(|i| {
            record(
                1 + i % 2,
                start + i * 1_000_000_007,
                &format!("job {i} done"),
            )
        })
        .collect();
    state.add(batch).unwrap();
    state.commit().unwrap();
    (dir, state, day)
}

pub(super) fn query(text: Option<&str>) -> LogQuery {
    LogQuery {
        project: None,
        since: 0,
        until: now_ns() + DAY_NS,
        min_level: None,
        text: text.map(str::to_owned),
        pattern: None,
        limit: 100,
        skip: 0,
    }
}

/// What the searches of the tests read: every hit, the hits of a text
/// query and of one project, and the histogram.
pub(super) fn read(
    dir: &Path,
) -> (
    Vec<LogHit>,
    Vec<LogHit>,
    Vec<LogHit>,
    Vec<u64>,
    PatternReport,
) {
    let mut one = query(None);
    one.project = Some(1);
    one.min_level = Some(LogLevel::Error);
    let patterns = PatternQuery {
        project: None,
        since: 0,
        until: now_ns() + DAY_NS,
        min_level: None,
        limit: 100,
    };
    (
        search_all(dir, &query(None)).unwrap(),
        search_all(dir, &query(Some(r#"job AND ("3 done" OR "17 done")"#))).unwrap(),
        search_all(dir, &one).unwrap(),
        histogram_all(dir, &query(None)).unwrap().counts,
        examples_aside(top_patterns(dir, &patterns).unwrap()),
    )
}

/// `report` without its examples: of the projects of a day, the one read
/// first gives the example, in no set order.
fn examples_aside(mut report: PatternReport) -> PatternReport {
    for row in &mut report.top {
        row.example.clear();
    }
    report
}

pub(super) fn hot(dir: &Path, day: Date) -> std::path::PathBuf {
    dir.join(day.to_string())
}

#[test]
fn a_day_turns_cold_and_reads_as_it_did_hot() {
    let (dir, mut state, day) = ten_days_back("cold-turn");
    let before = read(&dir);
    assert_eq!(before.0.len(), 30);
    assert_eq!(before.1.len(), 2);
    assert_eq!(before.2.len(), 8);
    assert_eq!(before.4.records, 30);

    state.turn_cold(day).unwrap();
    assert!(!hot(&dir, day).exists());
    let seg = layout::segment_dir(&dir, day, 1);
    let mut names: Vec<String> = std::fs::read_dir(&seg)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(
        names,
        ["1.dlc", "2.dlc", "patterns-1.json", "patterns-2.json"]
    );
    assert_eq!(read(&dir), before);
    state.close_all().unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_crash_after_any_step_loses_and_doubles_nothing() {
    for step in [Step::Sealed, Step::Written, Step::Renamed, Step::Trashed] {
        let (dir, mut state, day) = ten_days_back("cold-crash");
        let before = read(&dir);
        state.cold.crash = Some(step);
        let turned = state.turn_cold(day);
        assert_eq!(turned.is_err(), step == Step::Sealed, "{step:?}");
        assert_eq!(read(&dir), before, "read after a crash at {step:?}");
        drop(state);

        let mut state = State::new(dir.clone(), 30, no_overrides());
        state.recover_cold().unwrap();
        let cold = matches!(step, Step::Renamed | Step::Trashed);
        assert_eq!(hot(&dir, day).exists(), !cold, "{step:?}");
        assert_eq!(layout::seal(&hot(&dir, day)).unwrap(), None);
        let want: &[u32] = if cold { &[1] } else { &[] };
        assert_eq!(layout::segments(&dir, day).unwrap(), want, "{step:?}");
        assert_eq!(layout::temps(&dir, day).unwrap(), Vec::<u32>::new());
        assert_eq!(read(&dir), before, "read after recovery from {step:?}");

        if !cold {
            state.turn_cold(day).unwrap();
            assert!(!hot(&dir, day).exists());
            assert_eq!(layout::segments(&dir, day).unwrap(), [1]);
            assert_eq!(read(&dir), before, "read once cold after {step:?}");
        }
        state.close_all().unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

#[test]
fn a_log_for_a_cold_day_is_kept_hot_and_turns_cold_as_the_next_segment() {
    let (dir, mut state, day) = ten_days_back("cold-late");
    state.turn_cold(day).unwrap();
    let late = midnight(day) + 7_200_000_000_000 + 1;
    state.add(vec![record(1, late, "late job done")]).unwrap();
    state.commit().unwrap();
    let (all, ..) = read(&dir);
    assert_eq!(all.len(), 31);
    assert_eq!(
        all.iter().filter(|h| h.message == "late job done").count(),
        1
    );

    let mut first = query(None);
    first.limit = 1;
    first.until = late;
    let newest = search_all(&dir, &first).unwrap();
    assert_eq!(newest.len(), 1);
    assert_eq!(newest[0].message, "late job done");

    let before = read(&dir);
    assert_eq!(before.4.records, 31);
    state.turn_cold(day).unwrap();
    assert!(!hot(&dir, day).exists());
    assert_eq!(layout::segments(&dir, day).unwrap(), [1, 2]);
    assert_eq!(read(&dir), before);
    state.close_all().unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_log_for_a_day_turning_cold_keeps_it_hot() {
    let (dir, mut state, day) = ten_days_back("cold-unseal");
    state.start_job(day).unwrap();
    let late = midnight(day) + 1;
    state.add(vec![record(1, late, "late job done")]).unwrap();
    state.commit().unwrap();
    assert_eq!(layout::seal(&hot(&dir, day)).unwrap(), None);
    // Ending the job waits for its thread.
    let _stopped = state.end_job();
    assert!(state.cold.job.is_none());
    assert!(!state.cold.failed.contains(&day), "it may turn cold later");
    assert!(hot(&dir, day).exists());
    assert_eq!(layout::segments(&dir, day).unwrap(), Vec::<u32>::new());
    assert_eq!(layout::temps(&dir, day).unwrap(), Vec::<u32>::new());
    assert_eq!(read(&dir).0.len(), 31);
    state.close_all().unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_tick_turns_old_days_cold_and_leaves_expired_ones_to_retention() {
    let (dir, mut state, ten) = ten_days_back("cold-tick");
    let twenty = midnight(day_of(now_ns() - 20 * DAY_NS)) + 5;
    let two = midnight(day_of(now_ns() - 2 * DAY_NS)) + 5;
    state
        .add(vec![record(1, twenty, "old"), record(1, two, "new")])
        .unwrap();
    state.commit().unwrap();
    state.close_all().unwrap();

    let mut state = State::new(dir.clone(), 15, no_overrides());
    state.cold.hot_days = Some(3);
    assert_eq!(state.next_cold(now_ns()).unwrap(), Some(ten));
    state.cold_tick(now_ns()).unwrap();
    assert_eq!(state.cold.job.as_ref().map(|j| j.day), Some(ten));
    // What the next tick does once the thread is done, without waiting
    // for a tick to see it done.
    state.end_job().unwrap();
    state.cold_tick(now_ns()).unwrap();
    assert!(state.cold.job.is_none(), "no other day to turn cold");
    assert!(!hot(&dir, ten).exists());
    assert_eq!(layout::segments(&dir, ten).unwrap(), [1]);
    assert!(hot(&dir, day_of(twenty)).exists(), "retention deletes it");
    assert!(hot(&dir, day_of(two)).exists(), "still hot");
    assert_eq!(state.next_cold(now_ns()).unwrap(), None);
    state.close_all().unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}
