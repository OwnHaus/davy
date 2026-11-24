use super::{
    DayTally, EXAMPLE_BYTES, FILE, MAX_CELLS, MAX_PATTERNS, OTHER_SERVICES, SERVICE_BYTES,
    raw_bytes,
};
use crate::logs::{
    LogLevel, LogRecord, LogSource,
    patterns::{self, OTHER},
};
use crate::test_support::scratch;
use serde_json::json;

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
    assert!(dir.join(FILE).exists());
    let mut loaded = DayTally::load(&dir).unwrap();
    assert_eq!(count(&mut loaded, &record(1, None, &distinct(5000))), OTHER);
    assert_eq!(loaded.projects[&1].cells, MAX_PATTERNS + 1);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_day_without_counters_loads_empty() {
    let dir = scratch("tally-none");
    std::fs::create_dir_all(&dir).unwrap();
    assert!(DayTally::load(&dir).unwrap().projects.is_empty());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_save_writes_past_anything_at_a_fixed_temporary_name() {
    let dir = scratch("tally-temp");
    std::fs::create_dir_all(dir.join(format!("{FILE}.tmp"))).unwrap();
    let mut tally = DayTally::default();
    count(&mut tally, &record(1, None, "user 1 logged in"));
    tally.save(&dir).unwrap();
    let loaded = DayTally::load(&dir).unwrap();
    let entry = &loaded.projects[&1].patterns[&patterns::of("user 2 logged in").id];
    assert_eq!(entry.services[""][2][0], 1);
    let names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n != &format!("{FILE}.tmp"))
        .collect();
    assert_eq!(names, [FILE], "no temporary file is left");
    std::fs::remove_dir_all(&dir).unwrap();
}
