use super::{
    DayTally, EXAMPLE_BYTES, FLUSH_EVERY, MAX_CELLS, MAX_PATTERNS, OTHER_SERVICES, SERVICE_BYTES,
    file::{self, MAX_FILE_BYTES, name},
    pace, raw_bytes,
};
use crate::logs::{
    LogLevel, LogRecord, LogSource,
    patterns::{self, OTHER},
    store::{State, no_overrides},
};
use crate::test_support::scratch;
use crate::time::{day_of, now_ns};
use serde_json::json;
use std::{
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

#[test]
fn a_project_counts_1000_patterns_a_day_and_the_rest_under_other() {
    let mut tally = DayTally::default();
    let ids: Vec<u64> = (0..=MAX_PATTERNS)
        .map(|n| count(&mut tally, &record(1, None, &distinct(n))))
        .collect();
    assert!(ids[..MAX_PATTERNS].iter().all(|id| *id != OTHER));
    assert_eq!(ids[MAX_PATTERNS], OTHER);
    let again = count(&mut tally, &record(1, None, &distinct(7)));
    assert_eq!(again, ids[7], "a pattern counted before keeps its id");
    let other_project = count(&mut tally, &record(2, None, &distinct(MAX_PATTERNS)));
    assert_ne!(other_project, OTHER, "the bound is per project");
    assert_eq!(tally.projects[&1].patterns.len(), MAX_PATTERNS + 1);
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
    let ids: Vec<u64> = (0..=MAX_PATTERNS)
        .map(|n| pending(&mut tally, &record(1, None, &distinct(n))))
        .collect();
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
    for n in 0..=MAX_PATTERNS {
        count(&mut tally, &record(1, None, &distinct(n)));
    }
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
