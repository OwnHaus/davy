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
