use super::*;
use crate::logs::{LogRecord, store::no_overrides};
use crate::test_support::scratch;

fn record(source: LogSource) -> LogRecord {
    let at = now_ns();
    LogRecord {
        project_id: 1,
        timestamp: at,
        observed_at: at,
        source,
        level: LogLevel::Error,
        level_unknown: false,
        message: "log writer: commit: disk full".into(),
        service: None,
        trace_id: None,
        span_id: None,
        attributes: serde_json::Map::new(),
    }
}

#[test]
fn a_commit_of_davys_own_lines_alone_is_not_foreign() {
    let dir = scratch("log-writer-foreign");
    let mut state = State::new(dir.clone(), 30, no_overrides());
    assert!(!state.foreign());
    state.add(vec![record(LogSource::Davy)]).unwrap();
    assert!(
        !state.foreign(),
        "a failed commit would only lose Davy's own"
    );
    state.add(vec![record(LogSource::Otlp)]).unwrap();
    assert!(state.foreign());
    state.commit().unwrap();
    assert!(!state.foreign(), "a commit starts over");
    state.close_all().unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The lines of project 1 the log writer stored and those its rules
/// dropped (`true`) or would drop (`false`), once committed.
fn stored_and_dropped(
    rules: &[crate::drops::Stored],
    lines: Vec<LogRecord>,
) -> (u64, Vec<(i64, bool, u64)>) {
    let dir = scratch("log-writer-drops");
    let mut state = State::new(dir.clone(), 30, no_overrides());
    state.drops.set(crate::drops::Set::compile(rules));
    state.add(lines).unwrap();
    state.commit().unwrap();
    state.close_all().unwrap();
    let flush = state.log_days.take();
    let stored = flush
        .segments
        .iter()
        .flat_map(|s| &s.rows)
        .map(|r| r.count.records)
        .sum();
    let dropped = flush
        .drops
        .iter()
        .map(|(k, n)| (k.rule, k.live, n.records))
        .collect();
    std::fs::remove_dir_all(&dir).unwrap();
    (stored, dropped)
}

fn drop_rule(id: i64, live: bool, level: LogLevel) -> crate::drops::Stored {
    crate::drops::Stored {
        id,
        project: 1,
        position: id,
        live,
        by: crate::drops::By::Agent,
        spec: crate::drops::Spec {
            matches: crate::drops::Match::Level(level),
            service: None,
            keep: 0,
            any_level: false,
        },
        created_at: 0,
        changed_at: 0,
        expires_at: None,
    }
}

fn at_level(source: LogSource, level: LogLevel) -> LogRecord {
    LogRecord {
        level,
        level_unknown: false,
        ..record(source)
    }
}

#[test]
fn a_live_rule_keeps_its_lines_out_of_the_store_and_counts_them() {
    let lines = vec![
        at_level(LogSource::Otlp, LogLevel::Debug),
        at_level(LogSource::Otlp, LogLevel::Debug),
        at_level(LogSource::Otlp, LogLevel::Info),
    ];
    let rules = [drop_rule(7, true, LogLevel::Debug)];
    assert_eq!(stored_and_dropped(&rules, lines), (1, vec![(7, true, 2)]));
}

#[test]
fn a_shadow_rule_stores_every_line_and_counts_what_it_would_drop() {
    let lines = vec![
        at_level(LogSource::Otlp, LogLevel::Debug),
        at_level(LogSource::Otlp, LogLevel::Info),
    ];
    let rules = [drop_rule(7, false, LogLevel::Debug)];
    assert_eq!(stored_and_dropped(&rules, lines), (2, vec![(7, false, 1)]));
}

#[test]
fn a_shadow_rule_on_davys_own_project_hides_none_of_its_warnings_or_errors() {
    let lines = vec![
        at_level(LogSource::Davy, LogLevel::Warn),
        at_level(LogSource::Davy, LogLevel::Error),
        at_level(LogSource::Davy, LogLevel::Fatal),
    ];
    let rules = [drop_rule(7, false, LogLevel::Fatal)];
    assert_eq!(stored_and_dropped(&rules, lines), (3, vec![(7, false, 3)]));
}

#[test]
fn a_live_rule_on_davys_own_project_drops_what_it_names_warnings_too() {
    let lines = vec![
        at_level(LogSource::Davy, LogLevel::Info),
        at_level(LogSource::Davy, LogLevel::Warn),
        at_level(LogSource::Davy, LogLevel::Error),
    ];
    let mut rule = drop_rule(7, true, LogLevel::Warn);
    rule.by = crate::drops::By::Person;
    assert_eq!(stored_and_dropped(&[rule], lines), (1, vec![(7, true, 2)]));
}
