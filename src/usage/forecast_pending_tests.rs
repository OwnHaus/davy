//! A server that stores errors and whose first logs have no log basis
//! yet. Its forecast rests on errors alone, at low confidence, until the
//! logs give one.

use super::errors_only_tests::{day, errors_only};
use super::*;
use crate::time;

/// 10 log records of `api`, 1 869 raw bytes, on `on`.
fn ten_records(on: &str) -> RawDay {
    RawDay {
        day: day(on),
        projects: vec![("api".to_owned(), 1_869, false)],
    }
}

/// What [`errors_only`] gives at medium confidence, with logs pending
/// for `reason`.
fn pending(input: &Input, reason: &str) {
    let f = forecast(input, &WhatIf::default()).unwrap();
    let logs = f.logs.as_ref().unwrap();
    assert_eq!(
        (logs.basis, logs.bound(), f.days_left, f.full_on),
        (
            Basis::ErrorsOnly {
                lean: Lean::Benchmark(0),
                logs_pending: true
            },
            0,
            Some(7),
            Some(day("2026-12-01"))
        )
    );
    assert!(f.logs_pending());
    assert_eq!(f.confidence(false), Some(Confidence::Low));
    assert_eq!(
        f.explain(false).unwrap(),
        format!(
            "logs are not in this forecast yet, so it rests on errors alone and counts logs as 0: {reason}; confidence is low while logs are left out"
        )
    );
}

/// An old server whose first log line came today: the disk holds no
/// closed log day, so the log path waits for 2.
#[test]
fn a_first_log_line_on_an_old_server_keeps_the_errors_forecast() {
    let mut input = errors_only();
    input.raw = vec![ten_records("2026-11-24")];
    pending(
        &input,
        "closed log days since 2026-10-25, the first day no retention has cut: the disk has 0, a forecast needs 2",
    );
}

/// One log day of 61 kB on 2026-11-20 and nothing since: the log path
/// waits until retention cuts it.
#[test]
fn one_stray_log_day_keeps_the_errors_forecast() {
    let mut input = errors_only();
    input.raw = vec![ten_records("2026-11-20")];
    input.closed = vec![(day("2026-11-20"), 61_000)];
    pending(
        &input,
        "closed log days since 2026-10-25, the first day no retention has cut: the disk has 1, a forecast needs 2",
    );
}

/// Counting began 10 minutes ago and logs came: the log path waits for an
/// hour of them.
#[test]
fn logs_in_the_first_hour_keep_the_errors_forecast() {
    let mut input = errors_only();
    input.raw = vec![ten_records("2026-11-24")];
    input.counted_since = Some(input.now - 10 * (time::HOUR_NS / 60));
    pending(
        &input,
        "Davy has counted raw log bytes for 10 minutes; a first forecast needs an hour of them, or closed log days on disk",
    );
}

/// Without errors, logs with no basis give no forecast, as before.
#[test]
fn a_first_log_line_without_errors_gives_no_forecast() {
    let mut input = errors_only();
    input.raw = vec![ten_records("2026-11-24")];
    input.errors.clear();
    assert_eq!(
        forecast(&input, &WhatIf::default()).unwrap_err(),
        NoForecast::History(
            "closed log days since 2026-10-25, the first day no retention has cut: the disk has 0, a forecast needs 2"
                .to_owned()
        )
    );
}
