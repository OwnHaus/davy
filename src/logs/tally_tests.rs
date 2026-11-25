use super::Cut;
use super::{
    ADMIT_AFTER, DayTally, EXAMPLE_BYTES, FLUSH_EVERY, FREE_PATTERNS, MAX_CELLS, MAX_PATTERNS,
    OTHER_SERVICES, SERVICE_BYTES,
    file::{self, MAX_FILE_BYTES, name},
    open, pace, raw_bytes,
};
use crate::logs::{
    LogLevel, LogRecord, LogSource,
    patterns::{self, OTHER},
    store::{Overrides, State, no_overrides},
};
use crate::retention::Lock;
use crate::test_support::scratch;
use crate::time::{day_of, now_ns};
use serde_json::json;
use std::{
    collections::HashMap,
    path::Path,
    time::{Duration, Instant},
};

fn record(project_id: i64, service: Option<&str>, message: &str) -> LogRecord {
    LogRecord {
        project_id,
        timestamp: 0,
        observed_at: 0,
        source: LogSource::Logtail,
        level: LogLevel::Info,
        message: message.into(),
        service: service.map(Into::into),
        trace_id: None,
        span_id: None,
        attributes: serde_json::Map::new(),
    }
}

/// Counts `r` as pending, as the writer does before the commit.
fn pending(tally: &mut DayTally, r: &LogRecord) -> u64 {
    tally.count(r, patterns::of(&r.message), raw_bytes(r))
}

/// Counts `r` and settles it, as a commit that held does.
fn count(tally: &mut DayTally, r: &LogRecord) -> u64 {
    let id = pending(tally, r);
    tally.settle();
    id
}

/// A message with a pattern of its own for each `n`: digits would be masked.
fn distinct(n: usize) -> String {
    let word: String = format!("{n:04}")
        .bytes()
        .map(|d| char::from(b'a' + d - b'0'))
        .collect();
    format!("event {word} happened")
}

/// Counts a pattern of its own for each of `0..=MAX_PATTERNS` in project 1
/// with `how`, each as often as it takes to get a count of its own, and
/// returns the id of its last count.
fn fill(tally: &mut DayTally, how: fn(&mut DayTally, &LogRecord) -> u64) -> Vec<u64> {
    (0..=MAX_PATTERNS)
        .map(|n| {
            let r = record(1, None, &distinct(n));
            let times = if n < FREE_PATTERNS { 1 } else { ADMIT_AFTER };
            (1..times).for_each(|_| assert_eq!(how(tally, &r), OTHER));
            how(tally, &r)
        })
        .collect()
}

#[test]
fn a_project_counts_1000_patterns_a_day_and_the_rest_under_other() {
    let mut tally = DayTally::default();
    let ids = fill(&mut tally, count);
    assert!(ids[..MAX_PATTERNS].iter().all(|id| *id != OTHER));
    assert_eq!(ids[MAX_PATTERNS], OTHER);
    let again = count(&mut tally, &record(1, None, &distinct(7)));
    assert_eq!(again, ids[7], "a pattern counted before keeps its id");
    let other_project = count(&mut tally, &record(2, None, &distinct(MAX_PATTERNS)));
    assert_ne!(other_project, OTHER, "the bound is per project");
    assert_eq!(tally.projects[&1].patterns.len(), MAX_PATTERNS + 1);
}

#[test]
fn lines_seen_once_past_500_patterns_leave_the_room_to_lines_that_recur() {
    let mut tally = DayTally::default();
    for n in 0..FREE_PATTERNS {
        count(&mut tally, &record(1, None, &distinct(n)));
    }
    let junk: Vec<u64> = (FREE_PATTERNS..FREE_PATTERNS + 5000)
        .map(|n| count(&mut tally, &record(1, None, &distinct(n))))
        .collect();
    assert!(junk.iter().all(|id| *id == OTHER));
    let real = record(1, None, "payment captured");
    let ids: Vec<u64> = (0..3).map(|_| count(&mut tally, &real)).collect();
    assert_eq!(ids, [OTHER, OTHER, patterns::of("payment captured").id]);
    let project = &tally.projects[&1];
    assert_eq!(project.patterns.len(), FREE_PATTERNS + 2);
    assert_eq!(project.patterns[&OTHER].services[""][2][0], 5002);
}

#[test]
fn a_line_that_recurs_among_more_junk_than_is_tracked_still_gets_in() {
    let mut tally = DayTally::default();
    for n in 0..FREE_PATTERNS {
        count(&mut tally, &record(1, None, &distinct(n)));
    }
    let real = record(1, None, "payment captured");
    let mut junk = FREE_PATTERNS..;
    let mut ids = Vec::new();
    // 400 new lines before each sighting: the table fills on the way to
    // the third, which costs the line one sighting, so the fourth gets in.
    for _ in 0..4 {
        for n in junk.by_ref().take(400) {
            assert_eq!(count(&mut tally, &record(1, None, &distinct(n))), OTHER);
        }
        ids.push(count(&mut tally, &real));
    }
    let id = patterns::of("payment captured").id;
    assert_eq!(ids, [OTHER, OTHER, OTHER, id]);
}

#[test]
fn lines_of_known_patterns_leave_the_sightings_of_a_new_line_alone() {
    let mut tally = DayTally::default();
    for n in 0..FREE_PATTERNS {
        count(&mut tally, &record(1, None, &distinct(n)));
    }
    let real = record(1, None, "payment captured");
    let mut ids = vec![count(&mut tally, &real)];
    for n in FREE_PATTERNS..FREE_PATTERNS + 600 {
        assert_eq!(count(&mut tally, &record(1, None, &distinct(n))), OTHER);
    }
    for n in 0..FREE_PATTERNS {
        assert_ne!(count(&mut tally, &record(1, None, &distinct(n))), OTHER);
    }
    ids.extend((0..2).map(|_| count(&mut tally, &real)));
    let id = patterns::of("payment captured").id;
    assert_eq!(ids, [OTHER, OTHER, id]);
}

#[test]
fn services_past_10000_cells_count_under_other_services() {
    let mut tally = DayTally::default();
    for n in 0..MAX_CELLS {
        count(
            &mut tally,
            &record(1, Some(&format!("svc-{n}")), "job done"),
        );
    }
    count(&mut tally, &record(1, Some("late"), "job done"));
    count(&mut tally, &record(1, Some("svc-3"), "job done"));
    let entry = &tally.projects[&1].patterns[&patterns::of("job done").id];
    assert_eq!(entry.services.len(), MAX_CELLS + 1);
    assert_eq!(entry.services[OTHER_SERVICES][2][0], 1);
    assert!(!entry.services.contains_key("late"));
    assert_eq!(entry.services["svc-3"][2][0], 2);
}

#[test]
fn examples_and_service_names_are_cut() {
    let mut tally = DayTally::default();
    let long = format!("go {}", "é".repeat(200));
    let service = "s".repeat(300);
    count(&mut tally, &record(1, Some(&service), &long));
    let entry = &tally.projects[&1].patterns[&patterns::of(&long).id];
    assert_eq!(entry.example.len(), EXAMPLE_BYTES - 1);
    assert!(long.starts_with(&entry.example));
    let names: Vec<&str> = entry.services.keys().map(AsRef::as_ref).collect();
    assert_eq!(names, ["s".repeat(SERVICE_BYTES)]);
}

#[test]
fn templates_examples_and_services_are_kept_without_controls_or_reordering_marks() {
    let mut tally = DayTally::default();
    let r = record(1, Some("api\u{202E}"), "login \u{202E}nimda\u{9b} done");
    count(&mut tally, &r);
    let entry = &tally.projects[&1].patterns[&patterns::of(&r.message).id];
    assert_eq!(entry.template, "login \u{FFFD}nimda\u{FFFD} done");
    assert_eq!(entry.example, "login \u{FFFD}nimda\u{FFFD} done");
    let services: Vec<&str> = entry.services.keys().map(AsRef::as_ref).collect();
    assert_eq!(services, ["api\u{FFFD}"]);
    let long = "\u{1}".repeat(EXAMPLE_BYTES + 1);
    count(&mut tally, &record(1, None, &long));
    let entry = &tally.projects[&1].patterns[&patterns::of(&long).id];
    assert_eq!(
        entry.example,
        "\u{FFFD}".repeat(85),
        "still at most 256 bytes"
    );
}

#[test]
fn the_first_message_of_a_pattern_is_its_example() {
    let mut tally = DayTally::default();
    count(&mut tally, &record(1, None, "user 1 logged in"));
    count(&mut tally, &record(1, None, "user 2 logged in"));
    let entry = &tally.projects[&1].patterns[&patterns::of("user 9 logged in").id];
    assert_eq!(entry.example, "user 1 logged in");
    assert_eq!(entry.template, "user <NUM> logged in");
}

#[test]
fn pending_counts_take_room_and_show_only_once_settled() {
    let mut tally = DayTally::default();
    let ids = fill(&mut tally, pending);
    assert_eq!(ids[MAX_PATTERNS], OTHER);
    assert!(tally.projects[&1].patterns.is_empty());
    tally.settle();
    assert_eq!(tally.projects[&1].patterns.len(), MAX_PATTERNS + 1);
    let done = pending(&mut tally, &record(1, Some("api"), &distinct(3)));
    assert_eq!(done, ids[3]);
    tally.discard();
    let entry = &tally.projects[&1].patterns[&ids[3]];
    assert_eq!(entry.services.len(), 1);
    assert_eq!(entry.services[""][2][0], 1);
    assert_eq!(
        pending(&mut tally, &record(1, None, &distinct(5000))),
        OTHER
    );
}

#[test]
fn raw_bytes_are_the_message_ids_and_attributes_as_json() {
    let mut r = record(1, Some("api"), "hello");
    r.trace_id = Some("ab".into());
    r.attributes = json!({"k": 1}).as_object().unwrap().clone();
    assert_eq!(raw_bytes(&r), 5 + 3 + 2 + 7);
}

#[test]
fn counters_are_saved_and_loaded_with_their_bounds() {
    let dir = scratch("tally-file");
    std::fs::create_dir_all(&dir).unwrap();
    let mut tally = DayTally::default();
    fill(&mut tally, count);
    tally.save(&dir).unwrap();
    assert!(dir.join(name(1)).exists());
    let mut loaded = DayTally::load(&dir, None).unwrap().0;
    assert_eq!(count(&mut loaded, &record(1, None, &distinct(5000))), OTHER);
    assert_eq!(loaded.projects[&1].cells, MAX_PATTERNS + 1);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_day_without_counters_loads_empty() {
    let dir = scratch("tally-none");
    std::fs::create_dir_all(&dir).unwrap();
    assert!(DayTally::load(&dir, None).unwrap().0.projects.is_empty());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_save_writes_past_anything_at_a_fixed_temporary_name() {
    let dir = scratch("tally-temp");
    std::fs::create_dir_all(dir.join(format!("{}.tmp", name(1)))).unwrap();
    let mut tally = DayTally::default();
    count(&mut tally, &record(1, None, "user 1 logged in"));
    tally.save(&dir).unwrap();
    let loaded = DayTally::load(&dir, None).unwrap().0;
    let entry = &loaded.projects[&1].patterns[&patterns::of("user 2 logged in").id];
    assert_eq!(entry.services[""][2][0], 1);
    let names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n != &format!("{}.tmp", name(1)))
        .collect();
    assert_eq!(names, [name(1)], "no temporary file is left");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The records the file of `project` in `dir` counts.
fn saved(dir: &Path, project: u64) -> u64 {
    let counters = file::load(dir, project).unwrap().unwrap();
    let levels = counters.patterns.values().flat_map(|e| e.services.values());
    levels.flatten().map(|[records, _]| records).sum()
}

#[test]
fn a_flush_writes_only_the_projects_that_changed_when_they_are_due() {
    let dir = scratch("tally-flush");
    let now = now_ns();
    let day = dir.join(day_of(now).to_string());
    let line = |project| LogRecord {
        timestamp: now,
        observed_at: now,
        ..record(project, None, "job done")
    };
    let mut state = State::new(dir.clone(), 30, no_overrides());
    state.add(vec![line(1), line(2)]).unwrap();
    state.commit().unwrap();
    let start = Instant::now();
    state.flush_tallies(start);
    assert_eq!((saved(&day, 1), saved(&day, 2)), (1, 1));
    std::fs::remove_file(day.join(name(2))).unwrap();
    state.add(vec![line(1)]).unwrap();
    state.commit().unwrap();
    state.flush_tallies(start + FLUSH_EVERY / 2);
    assert_eq!(saved(&day, 1), 1, "not due yet");
    state.flush_tallies(start + FLUSH_EVERY);
    assert_eq!(saved(&day, 1), 2);
    state.close_all().unwrap();
    assert!(!day.join(name(2)).exists(), "project 2 did not change");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_big_file_is_written_less_often() {
    assert_eq!(pace(0), FLUSH_EVERY);
    assert_eq!(pace(150_000), Duration::from_secs(15));
    assert_eq!(pace(3_500_000), Duration::from_secs(350));
}

#[test]
fn a_file_past_the_bounds_or_too_big_is_not_read() {
    let dir = scratch("tally-bounds");
    std::fs::create_dir_all(&dir).unwrap();
    let file_of = |example: &str| {
        let levels = [[0, 0], [0, 0], [1, 9], [0, 0], [0, 0], [0, 0]];
        let entry = json!({"template": "a", "example": example, "services": {"": levels}});
        json!({"version": 1, "patterns": {"5": entry}}).to_string()
    };
    std::fs::write(dir.join(name(1)), file_of("a")).unwrap();
    assert_eq!(
        file::load(&dir, 1).unwrap().unwrap().patterns[&5].services[""][2],
        [1, 9]
    );
    std::fs::write(dir.join(name(1)), file_of(&"a".repeat(EXAMPLE_BYTES + 1))).unwrap();
    let e = format!("{:#}", file::load(&dir, 1).err().unwrap());
    assert!(e.ends_with(": an example of more than 256 bytes"), "{e}");
    let big = std::fs::File::create(dir.join(name(2))).unwrap();
    big.set_len(MAX_FILE_BYTES + 1).unwrap();
    let e = format!("{:#}", file::load(&dir, 2).err().unwrap());
    assert!(e.ends_with(": over 67108864 bytes"), "{e}");
    std::fs::remove_dir_all(&dir).unwrap();
}

const DAY: i64 = 86_400_000_000_000;

/// A line of project 1 stamped `at` and seen `now`.
fn line_at(at: i64, now: i64) -> LogRecord {
    LogRecord {
        timestamp: at,
        observed_at: now,
        ..record(1, None, "job done")
    }
}

#[test]
fn a_shutdown_writes_the_counters_of_a_day_a_failed_commit_dropped() {
    let dir = scratch("tally-shutdown-dropped");
    let now = now_ns();
    let mut state = State::new(dir.clone(), 30, no_overrides());
    state.add(vec![line_at(now, now)]).unwrap();
    state.commit().unwrap();
    // As a failed commit does: the writer goes, the counted logs stay.
    state.discard(day_of(now));
    state.close_all().unwrap();
    assert_eq!(saved(&dir.join(day_of(now).to_string()), 1), 1);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_shutdown_closes_every_day_and_keeps_what_it_could_not_write() {
    let dir = scratch("tally-shutdown-failed");
    let now = now_ns();
    let earlier = now - DAY;
    let mut state = State::new(dir.clone(), 30, no_overrides());
    state
        .add(vec![line_at(earlier, now), line_at(now, now)])
        .unwrap();
    state.commit().unwrap();
    state
        .add(vec![line_at(earlier, now), line_at(now, now)])
        .unwrap();
    let old = dir.join(day_of(earlier).to_string());
    std::fs::rename(&old, dir.join("gone")).unwrap();
    assert!(state.close_all().is_err());
    assert!(state.open.is_empty(), "a failed day does not stop the rest");
    assert_eq!(saved(&dir.join(day_of(now).to_string()), 1), 2);
    std::fs::create_dir(&old).unwrap();
    state.flush_tallies(Instant::now());
    // The lost commit took its count; the one before it holds.
    assert_eq!(saved(&old, 1), 1);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn only_the_first_of_a_run_of_failed_writes_is_reported() {
    let mut tally = DayTally::default();
    let reported = [true, true, false, true, true].map(|failed| tally.note(failed));
    assert_eq!(reported, [true, false, false, true, false]);
}

#[test]
fn a_sender_that_sends_to_one_day_after_another_does_not_make_files_be_written() {
    use std::os::unix::fs::MetadataExt;
    let dir = scratch("tally-cycle");
    let now = now_ns();
    let days: Vec<i64> = (0..4).map(|n| now - n * DAY).collect();
    let first = dir.join(day_of(days[0]).to_string());
    let mut state = State::new(dir.clone(), 30, no_overrides());
    let full: Vec<LogRecord> = (0..FREE_PATTERNS)
        .map(|n| LogRecord {
            timestamp: days[0],
            observed_at: now,
            ..record(1, None, &distinct(n))
        })
        .collect();
    state.add(full).unwrap();
    state.commit().unwrap();
    let start = Instant::now();
    state.flush_tallies(start);
    let len = std::fs::metadata(first.join(name(1))).unwrap().len();
    let inode = |dir: &Path| std::fs::metadata(dir.join(name(1))).unwrap().ino();
    let mut inodes = vec![inode(&first)];
    // Each line opens a day the writer does not have open, which closes
    // the one written longest ago.
    for _ in 0..5 {
        for at in &days {
            state.add(vec![line_at(*at, now)]).unwrap();
            state.commit().unwrap();
            inodes.push(inode(&first));
        }
    }
    inodes.dedup();
    assert_eq!(inodes.len(), 1, "written once in 20 lines");
    assert_eq!(saved(&first, 1), 500);
    state.flush_tallies(start + FLUSH_EVERY);
    let paced = pace(usize::try_from(len).unwrap());
    state.flush_tallies(start + paced);
    assert_eq!(saved(&first, 1), 505, "written again at its pace");
    state.close_all().unwrap();
    for day in &days[1..] {
        assert_eq!(saved(&dir.join(day_of(*day).to_string()), 1), 5);
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn past_3_closed_days_the_one_closed_first_is_written_and_let_go() {
    let dir = scratch("tally-held");
    let now = now_ns();
    let days: Vec<i64> = (0..7).map(|n| now - n * DAY).collect();
    let mut state = State::new(dir.clone(), 30, no_overrides());
    for at in &days {
        state.add(vec![line_at(*at, now)]).unwrap();
        state.commit().unwrap();
    }
    assert_eq!(state.tallies().len(), 6, "3 open and 3 closed");
    let files: Vec<bool> = days
        .iter()
        .map(|at| dir.join(day_of(*at).to_string()).join(name(1)).exists())
        .collect();
    assert_eq!(files, [true, false, false, false, false, false, false]);
    assert_eq!(saved(&dir.join(day_of(days[0]).to_string()), 1), 1);
    state.close_all().unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The names in `dir`, sorted.
fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort_unstable();
    names
}

/// Writes counters of two lines of project 1 in `dir` and returns the
/// bytes of the file.
fn two_lines(dir: &Path) -> Vec<u8> {
    std::fs::create_dir_all(dir).unwrap();
    let mut tally = DayTally::default();
    count(&mut tally, &record(1, None, "job done"));
    count(&mut tally, &record(1, None, "job done"));
    tally.save(dir).unwrap();
    std::fs::read(dir.join(name(1))).unwrap()
}

fn chmod(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// Counts a line of project 1 now in `state` as a commit that held does,
/// with the rights of `path` set to `mode` while the writer opens the day,
/// and returns whether the day's counters restarted.
fn count_with(state: &mut State, path: &Path, mode: u32) -> bool {
    let now = now_ns();
    chmod(path, mode);
    let line = line_at(now, now);
    state.tally(day_of(now), &line, raw_bytes(&line));
    chmod(path, 0o700);
    let mut tallies = state.tallies();
    let tally = tallies.get_mut(&day_of(now)).unwrap();
    tally.settle();
    tally.restarted(Some(1))
}

#[test]
fn a_file_the_writer_may_not_read_is_neither_set_aside_nor_written_over() {
    let dir = scratch("tally-denied");
    let day = dir.join(day_of(now_ns()).to_string());
    let before = two_lines(&day);
    let mut state = State::new(dir.clone(), 30, no_overrides());
    let restarted = count_with(&mut state, &day.join(name(1)), 0o000);
    state.flush_tallies(Instant::now() + Duration::from_secs(3600));
    state.close_all().unwrap();
    assert!(restarted);
    assert_eq!(names_in(&day), [name(1)]);
    assert_eq!(std::fs::read(day.join(name(1))).unwrap(), before);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_day_whose_files_cannot_be_listed_writes_none_of_them() {
    let dir = scratch("tally-unlisted");
    let day = dir.join(day_of(now_ns()).to_string());
    let before = two_lines(&day);
    let mut state = State::new(dir.clone(), 30, no_overrides());
    let restarted = count_with(&mut state, &day, 0o300);
    state.flush_tallies(Instant::now() + Duration::from_secs(3600));
    state.close_all().unwrap();
    assert!(restarted);
    assert_eq!(names_in(&day), [name(1)]);
    assert_eq!(std::fs::read(day.join(name(1))).unwrap(), before);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_symlink_in_place_of_a_file_is_not_followed_and_is_set_aside() {
    let dir = scratch("tally-symlink");
    let (day, elsewhere) = (dir.join("day"), dir.join("elsewhere"));
    let before = two_lines(&elsewhere);
    std::fs::create_dir_all(&day).unwrap();
    std::os::unix::fs::symlink(elsewhere.join(name(1)), day.join(name(1))).unwrap();
    let (tally, unread) = DayTally::load(&day, None).unwrap();
    let e = format!("{:#}", unread[0].1);
    assert!(e.ends_with(": not a regular file"), "{e}");
    assert_eq!((tally.projects.len(), unread.len()), (0, 1));
    let tally = open(day_of(now_ns()), &day);
    assert!(tally.restarted(Some(1)));
    let names = names_in(&day);
    assert!(
        names[0].starts_with(&format!("{}.unread-", name(1))),
        "{names:?}"
    );
    assert_eq!(names.len(), 1);
    assert_eq!(std::fs::read(elsewhere.join(name(1))).unwrap(), before);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn opening_a_day_removes_the_temporary_files_a_crash_left() {
    let dir = scratch("tally-crash");
    two_lines(&dir);
    let left = format!("{}.{}.tmp", name(2), uuid::Uuid::new_v4());
    std::fs::write(dir.join(left), "{").unwrap();
    std::fs::write(dir.join("other.tmp"), "").unwrap();
    let tally = open(day_of(now_ns()), &dir);
    assert_eq!(names_in(&dir), ["other.tmp".to_owned(), name(1)]);
    assert_eq!(tally.projects.len(), 1);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_cut_removes_the_temporary_and_set_aside_copies_of_its_projects() {
    let dir = scratch("tally-cut-copies");
    two_lines(&dir);
    let copies = [
        name(2),
        format!("{}.{}.tmp", name(2), uuid::Uuid::new_v4()),
        format!("{}.unread-123", name(2)),
    ];
    for copy in &copies {
        std::fs::write(dir.join(copy), "{").unwrap();
    }
    file::remove(&dir, &Cut::Only([2].into())).unwrap();
    assert_eq!(names_in(&dir), [name(1)]);
    for copy in &copies {
        std::fs::write(dir.join(copy), "{").unwrap();
    }
    file::remove(&dir, &Cut::AllBut([2].into())).unwrap();
    assert_eq!(
        names_in(&dir),
        copies,
        "project 1 went, project 2 stayed whole"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_retention_cut_drops_the_counts_of_its_projects_not_written_yet() {
    let dir = scratch("tally-cut-unwritten");
    let now = now_ns();
    let old = now - 5 * DAY;
    let day = dir.join(day_of(old).to_string());
    let line = |project| LogRecord {
        project_id: project,
        ..line_at(old, now)
    };
    let read = || Ok(HashMap::from([(2, 3)]));
    let mut state = State::new(dir.clone(), 30, Overrides::new(read, Lock::default()));
    state.add(vec![line(1), line(1), line(2)]).unwrap();
    state.commit().unwrap();
    assert!(!day.join(name(1)).exists(), "no file is written yet");
    state.retention(now, Duration::MAX).unwrap();
    state.flush_tallies(Instant::now() + Duration::from_secs(3600));
    assert_eq!(saved(&day, 1), 2);
    assert!(
        !day.join(name(2)).exists(),
        "the counts of project 2 went with its logs"
    );
    state.close_all().unwrap();
    assert!(!day.join(name(2)).exists());
    std::fs::remove_dir_all(&dir).unwrap();
}
