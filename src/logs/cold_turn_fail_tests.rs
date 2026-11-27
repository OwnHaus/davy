//! A day turning cold that fails at each step: what it leaves, what reads
//! see, and how the next tick or start finishes it.

use super::{
    super::layout,
    Step,
    tests::{hot, read, record, ten_days_back},
};
use crate::logs::store::{Overrides, State, no_overrides};
use crate::retention::Lock;
use crate::time::{midnight, now_ns};
use jiff::civil::Date;
use std::{
    collections::HashMap,
    os::unix::fs::PermissionsExt as _,
    path::Path,
    time::{Duration, Instant},
};

const HALFWAY: Duration = Duration::from_secs(60);

/// A state on `dir` whose retention lock another process, `cli`, holds:
/// taking it fails at once.
fn locked(dir: &Path) -> (State, std::fs::File) {
    let cli = std::fs::File::open(dir).unwrap();
    cli.lock().unwrap();
    let lock = Lock::default().with_dir(dir).waiting(Duration::ZERO);
    let overrides = Overrides::new(|| Ok(HashMap::new()), lock);
    (State::new(dir.to_owned(), 30, overrides), cli)
}

fn late(day: Date) -> crate::logs::LogRecord {
    record(1, midnight(day) + 7_200_000_000_000 + 1, "late job done")
}

fn lates(dir: &Path) -> usize {
    let all = read(dir).0;
    all.iter().filter(|h| h.message == "late job done").count()
}

#[test]
fn a_failure_after_the_rename_keeps_the_seal_and_the_next_tick_finishes() {
    let (dir, mut state, day) = ten_days_back("cold-unfinished");
    let before = read(&dir);
    std::fs::write(dir.join(".trash"), b"").unwrap();
    let turned = state.turn_cold(day).unwrap_err();
    assert!(
        format!("{turned:#}").contains("segment 1 is in place"),
        "{turned:#}"
    );
    assert!(
        !state.cold.failed.contains(&day),
        "the next tick tries again"
    );
    assert!(hot(&dir, day).exists());
    assert_eq!(layout::seal(&hot(&dir, day)).unwrap(), Some(1));
    assert_eq!(layout::segments(&dir, day).unwrap(), [1]);
    assert_eq!(read(&dir), before, "the segment alone is read");

    std::fs::remove_file(dir.join(".trash")).unwrap();
    state.cold.hot_days = Some(3);
    state.cold.recovered = true;
    // The try a minute later.
    state.cold.next_scan = Instant::now();
    state.cold_tick(now_ns()).unwrap();
    assert!(state.cold.job.is_none(), "step 4 needs no conversion");
    assert!(!hot(&dir, day).exists());
    assert_eq!(layout::segments(&dir, day).unwrap(), [1]);
    assert_eq!(read(&dir), before);
    state.close_all().unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_start_after_a_failure_past_the_rename_finishes_the_day() {
    let (dir, mut state, day) = ten_days_back("cold-unfinished-start");
    let before = read(&dir);
    std::fs::write(dir.join(".trash"), b"").unwrap();
    state.turn_cold(day).unwrap_err();
    drop(state);

    // Still failing: the day is read from its segment, and each tick
    // tries step 4 again.
    let mut state = State::new(dir.clone(), 30, no_overrides());
    state.recover_cold().unwrap();
    assert!(!state.cold.failed.contains(&day));
    assert!(state.cold.unfinished.contains(&day));
    assert_eq!(layout::seal(&hot(&dir, day)).unwrap(), Some(1));
    assert_eq!(read(&dir), before);
    drop(state);

    std::fs::remove_file(dir.join(".trash")).unwrap();
    let mut state = State::new(dir.clone(), 30, no_overrides());
    state.recover_cold().unwrap();
    assert!(!hot(&dir, day).exists());
    assert_eq!(layout::segments(&dir, day).unwrap(), [1]);
    assert_eq!(read(&dir), before);
    state.close_all().unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_failed_recovery_is_tried_on_each_tick_and_no_day_turns_cold_before() {
    let (dir, mut state, day) = ten_days_back("cold-recover-locked");
    state.cold.crash = Some(Step::Renamed);
    state.turn_cold(day).unwrap();
    drop(state);
    let before = read(&dir);

    let (mut state, cli) = locked(&dir);
    state.cold.hot_days = Some(3);
    assert!(state.recover_cold().is_err());
    let ticked = state.cold_tick(now_ns()).unwrap_err();
    assert!(format!("{ticked:#}").contains("before any day turns cold"));
    assert!(state.cold.job.is_none());
    assert!(!state.cold.recovered);
    assert_eq!(read(&dir), before);

    // A log for the day while recovery fails: the seal names a segment
    // that exists, so the hot day goes first and the log starts a new one.
    state.add(vec![late(day)]).unwrap();
    state.commit().unwrap();
    assert_eq!(lates(&dir), 1);
    assert_eq!(read(&dir).0.len(), 31);
    assert_eq!(layout::segments(&dir, day).unwrap(), [1]);
    assert_eq!(layout::seal(&hot(&dir, day)).unwrap(), None);

    drop(cli);
    state.close_all().unwrap();
    state.cold.next_scan = Instant::now();
    state.cold_tick(now_ns()).unwrap();
    assert!(state.cold.recovered);
    assert_eq!(state.cold.job.as_ref().map(|j| j.day), Some(day));
    state.end_job().unwrap();
    assert!(!hot(&dir, day).exists());
    assert_eq!(layout::segments(&dir, day).unwrap(), [1, 2]);
    assert_eq!(lates(&dir), 1);
    assert_eq!(read(&dir).0.len(), 31);
    state.close_all().unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_day_that_fails_recovery_does_not_hold_back_the_others() {
    let (dir, mut state, bad) = ten_days_back("cold-recover-one-bad");
    let good = bad.yesterday().unwrap();
    state.add(vec![late(good)]).unwrap();
    state.commit().unwrap();
    state.cold.crash = Some(Step::Renamed);
    state.turn_cold(bad).unwrap();
    state.turn_cold(good).unwrap();
    drop(state);
    let seal = hot(&dir, bad).join("cold-seal");
    std::fs::set_permissions(&seal, std::fs::Permissions::from_mode(0o000)).unwrap();

    let mut state = State::new(dir.clone(), 30, no_overrides());
    state.recover_cold().unwrap();
    assert!(state.cold.recovered);
    assert!(state.cold.failed.contains(&bad), "its seal cannot be read");
    assert!(hot(&dir, bad).exists());
    assert!(!state.cold.failed.contains(&good));
    assert!(!hot(&dir, good).exists());
    assert_eq!(layout::segments(&dir, good).unwrap(), [1]);
    std::fs::set_permissions(&seal, std::fs::Permissions::from_mode(0o600)).unwrap();
    state.close_all().unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_log_for_a_day_with_a_stale_seal_and_no_job_takes_the_seal_off() {
    let (dir, mut state, day) = ten_days_back("cold-stale-seal");
    state.close_all().unwrap();
    layout::write_seal(&hot(&dir, day), 7).unwrap();
    assert_eq!(read(&dir).0.len(), 30, "a seal for no segment reads hot");
    state.add(vec![late(day)]).unwrap();
    state.commit().unwrap();
    assert_eq!(layout::seal(&hot(&dir, day)).unwrap(), None);
    assert_eq!(read(&dir).0.len(), 31);
    assert_eq!(lates(&dir), 1);
    state.close_all().unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_log_while_the_day_turns_cold_keeps_it_hot_and_whole() {
    let (dir, mut state, day) = ten_days_back("cold-late-paused");
    let (there, go) = state.cold.pause_next();
    state.start_job(day).unwrap();
    there.recv_timeout(HALFWAY).unwrap();
    assert_eq!(layout::temps(&dir, day).unwrap(), [1]);
    state.add(vec![late(day)]).unwrap();
    state.commit().unwrap();
    assert_eq!(layout::seal(&hot(&dir, day)).unwrap(), None);
    go.send(()).unwrap();
    state.end_job().unwrap();
    assert!(!state.cold.failed.contains(&day), "it may turn cold later");
    assert!(hot(&dir, day).exists());
    assert_eq!(layout::segments(&dir, day).unwrap(), Vec::<u32>::new());
    assert_eq!(layout::temps(&dir, day).unwrap(), Vec::<u32>::new());
    assert_eq!(read(&dir).0.len(), 31);
    assert_eq!(lates(&dir), 1);
    state.close_all().unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_conversion_that_fails_leaves_the_day_hot_until_the_next_start() {
    let (dir, mut state, day) = ten_days_back("cold-convert-fails");
    let before = read(&dir);
    let (there, go) = state.cold.pause_next();
    state.start_job(day).unwrap();
    there.recv_timeout(HALFWAY).unwrap();
    std::fs::remove_dir_all(layout::temp_dir(&dir, day, 1)).unwrap();
    go.send(()).unwrap();
    let ended = state.end_job().unwrap_err();
    assert!(format!("{ended:#}").contains("left hot"), "{ended:#}");
    assert!(state.cold.failed.contains(&day));
    assert_eq!(layout::seal(&hot(&dir, day)).unwrap(), None);
    assert_eq!(layout::segments(&dir, day).unwrap(), Vec::<u32>::new());
    assert_eq!(layout::temps(&dir, day).unwrap(), Vec::<u32>::new());
    assert_eq!(read(&dir), before);
    state.cold.hot_days = Some(3);
    assert_eq!(state.next_cold(now_ns()).unwrap(), None);
    state.close_all().unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_day_whose_pattern_counters_cannot_be_saved_is_left_out() {
    let (dir, mut state, day) = ten_days_back("cold-held-tally");
    state.add(vec![late(day)]).unwrap();
    state.commit().unwrap();
    // A directory where the counters of project 1 go cannot be replaced.
    let counters = hot(&dir, day).join("patterns-1.json");
    let _gone = std::fs::remove_file(&counters);
    std::fs::create_dir_all(counters.join("x")).unwrap();
    let started = state.start_job(day).unwrap_err();
    assert!(
        format!("{started:#}").contains("pattern counters"),
        "{started:#}"
    );
    assert!(state.cold.failed.contains(&day));
    assert!(state.cold.job.is_none());
    assert_eq!(layout::seal(&hot(&dir, day)).unwrap(), None);
    state.cold.hot_days = Some(3);
    assert_eq!(state.next_cold(now_ns()).unwrap(), None);
    std::fs::remove_dir_all(&counters).unwrap();
    state.close_all().unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_lock_held_at_the_rename_leaves_the_day_hot_for_a_later_try() {
    let (dir, state, day) = ten_days_back("cold-locked-rename");
    drop(state);
    let before = read(&dir);
    let (mut state, cli) = locked(&dir);
    state.cold.hot_days = Some(3);
    state.cold.recovered = true;
    state.cold_tick(now_ns()).unwrap();
    assert_eq!(state.cold.job.as_ref().map(|j| j.day), Some(day));
    let ended = state.end_job().unwrap_err();
    assert!(format!("{ended:#}").contains("retention lock"), "{ended:#}");
    assert!(
        !state.cold.failed.contains(&day),
        "a later try takes it again"
    );
    assert_eq!(layout::seal(&hot(&dir, day)).unwrap(), None);
    assert_eq!(layout::temps(&dir, day).unwrap(), Vec::<u32>::new());
    assert_eq!(layout::segments(&dir, day).unwrap(), Vec::<u32>::new());
    assert_eq!(read(&dir), before);
    // No new conversion on the next commit while the lock may be held.
    state.cold_tick(now_ns()).unwrap();
    assert!(state.cold.job.is_none());

    drop(cli);
    state.cold.next_scan = Instant::now();
    state.cold_tick(now_ns()).unwrap();
    assert_eq!(state.cold.job.as_ref().map(|j| j.day), Some(day));
    state.end_job().unwrap();
    assert!(!hot(&dir, day).exists());
    assert_eq!(layout::segments(&dir, day).unwrap(), [1]);
    assert_eq!(read(&dir), before);
    state.close_all().unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_failure_of_step_4_that_stays_is_tried_each_minute_until_it_works() {
    let (dir, mut state, day) = ten_days_back("cold-unfinished-stays");
    let before = read(&dir);
    std::fs::write(dir.join(".trash"), b"").unwrap();
    state.turn_cold(day).unwrap_err();
    state.cold.hot_days = Some(3);
    state.cold.recovered = true;
    for tick in 0..3 {
        state.cold.next_scan = Instant::now();
        let ticked = state.cold_tick(now_ns()).unwrap_err();
        assert!(format!("{ticked:#}").contains("finishing"), "{ticked:#}");
        assert!(!state.cold.failed.contains(&day), "tick {tick}");
        assert!(state.cold.job.is_none(), "tick {tick}: no other day starts");
        // One warning a tick at most: the next commit waits.
        state.cold_tick(now_ns()).unwrap();
        assert_eq!(layout::seal(&hot(&dir, day)).unwrap(), Some(1));
        assert_eq!(read(&dir), before, "tick {tick}");
    }

    std::fs::remove_file(dir.join(".trash")).unwrap();
    state.cold.next_scan = Instant::now();
    state.cold_tick(now_ns()).unwrap();
    assert!(state.cold.job.is_none(), "step 4 needs no conversion");
    assert!(!hot(&dir, day).exists());
    assert_eq!(layout::segments(&dir, day).unwrap(), [1]);
    assert_eq!(read(&dir), before);
    state.close_all().unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_late_log_for_a_day_whose_step_4_failed_is_taken_once_the_fault_is_gone() {
    let (dir, mut state, day) = ten_days_back("cold-unfinished-late");
    std::fs::write(dir.join(".trash"), b"").unwrap();
    state.turn_cold(day).unwrap_err();
    state.cold.hot_days = Some(3);
    state.cold.recovered = true;
    state.cold.next_scan = Instant::now();
    state.cold_tick(now_ns()).unwrap_err();
    assert!(state.add(vec![late(day)]).is_err());
    assert_eq!(lates(&dir), 0);
    assert_eq!(read(&dir).0.len(), 30);

    std::fs::remove_file(dir.join(".trash")).unwrap();
    state.add(vec![late(day)]).unwrap();
    state.commit().unwrap();
    assert_eq!(lates(&dir), 1);
    assert_eq!(read(&dir).0.len(), 31);
    state.cold.next_scan = Instant::now();
    state.cold_tick(now_ns()).unwrap();
    assert_eq!(layout::seal(&hot(&dir, day)).unwrap(), None);
    state.close_all().unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}
