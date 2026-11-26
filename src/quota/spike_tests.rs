//! Spike buckets: a quiet project may send ten seconds of its rate at
//! once, then its rate. Errors and logs have a bucket each, other signals
//! none, and one request makes a project wait at most ten seconds.

use super::*;
use crate::usage::Count;
use std::collections::HashMap;

/// 2026-11-26 12:00 UTC.
const NOON: i64 = 1_795_694_400_000_000_000;
const MS: i64 = 1_000_000;
const SECOND: i64 = 1_000 * MS;

fn rate(n: u64) -> Settings {
    Settings {
        spike: Spike::Rate(n),
        ..Settings::default()
    }
}

fn logs(n: u64) -> Settings {
    Settings {
        spike: Spike::Off,
        spike_logs: Spike::Rate(n),
        ..Settings::default()
    }
}

#[test]
fn a_quiet_project_sends_a_burst_of_ten_seconds_then_waits_for_its_rate() {
    let g = Gate::default();
    g.load(HashMap::from([(1, rate(10))]), NOON);
    g.used(1, Signal::Errors, Count::new(99, 1), NOON);
    assert_eq!(g.limits(1, NOON).spike, [None, None]);
    g.used(1, Signal::Errors, Count::new(1, 1), NOON);
    assert_eq!(g.limits(1, NOON).spike, [Some(1), None]);
    assert_eq!(g.limits(1, NOON + 99 * MS).spike, [Some(1), None]);
    assert_eq!(g.limits(1, NOON + 100 * MS).spike, [None, None]);
    assert_eq!(g.limits(2, NOON).spike, [None, None]);
}

#[test]
fn one_request_bigger_than_the_bucket_makes_the_project_wait_one_burst_at_most() {
    let g = Gate::default();
    g.load(HashMap::from([(1, logs(1))]), NOON);
    g.used(1, Signal::Logs, Count::new(2000, 50_000), NOON);
    assert_eq!(g.limits(1, NOON).spike, [None, Some(10)]);
    assert_eq!(g.limits(1, NOON + 9 * SECOND).spike, [None, Some(1)]);
    assert_eq!(g.limits(1, NOON + 10 * SECOND).spike, [None, None]);
    g.load(HashMap::from([(1, rate(1))]), NOON);
    g.used(1, Signal::Errors, Count::new(2000, 1), NOON);
    g.used(1, Signal::Errors, Count::new(2000, 1), NOON + 5 * SECOND);
    assert_eq!(g.limits(1, NOON + 5 * SECOND).spike, [Some(10), None]);
    assert_eq!(g.limits(1, NOON + 15 * SECOND).spike, [None, None]);
}

#[test]
fn a_log_burst_never_refuses_errors_and_other_signals_meet_no_bucket() {
    let g = Gate::default();
    let burst = usize::try_from(DEFAULT_SPIKE).unwrap() * 10;
    g.used(7, Signal::Logs, Count::new(10_000_000, 1), NOON);
    g.used(7, Signal::Sessions, Count::new(burst, 1), NOON);
    g.used(7, Signal::Checkins, Count::new(burst, 1), NOON);
    g.used(7, Signal::Feedback, Count::new(burst, 1), NOON);
    assert_eq!(g.limits(7, NOON).spike, [None, None]);
    g.used(7, Signal::Errors, Count::new(burst - 1, 1), NOON);
    assert_eq!(g.limits(7, NOON).spike, [None, None]);
    g.used(7, Signal::Errors, Count::new(1, 1), NOON);
    assert_eq!(g.limits(7, NOON).spike, [Some(1), None]);
    assert_eq!(g.limits(7, NOON + SECOND / 4000).spike, [None, None]);
    g.load(HashMap::from([(7, logs(100))]), NOON);
    g.used(7, Signal::Logs, Count::new(5000, 1), NOON);
    let l = g.limits(7, NOON);
    assert_eq!(l.spike, [None, Some(10)]);
    assert!(!l.refuses(Signal::Errors) && !l.refuses(Signal::Checkins));
}

#[test]
fn off_never_refuses_and_a_new_rate_starts_with_a_full_bucket() {
    let g = Gate::default();
    let off = Settings {
        spike: Spike::Off,
        ..Settings::default()
    };
    g.load(HashMap::from([(1, off)]), NOON);
    g.used(1, Signal::Errors, Count::new(10_000_000, 1), NOON);
    assert_eq!(g.limits(1, NOON).spike, [None, None]);
    g.load(HashMap::from([(1, rate(1))]), NOON);
    g.used(1, Signal::Errors, Count::new(11, 1), NOON);
    assert_eq!(g.limits(1, NOON).spike, [Some(2), None]);
    g.load(HashMap::new(), NOON);
    assert_eq!(g.limits(1, NOON).spike, [None, None]);
}

#[test]
fn a_quota_refuses_before_the_bucket_and_each_has_its_reason() {
    let l = Limits {
        quota: [true, false],
        midnight: 3600,
        spike: [Some(3), Some(4)],
    };
    assert_eq!(
        (
            l.spiked(Signal::Errors),
            l.refuses(Signal::Errors),
            l.reason(Signal::Errors)
        ),
        (None, true, "davy_quota")
    );
    assert_eq!(
        (
            l.spiked(Signal::Logs),
            l.refuses(Signal::Logs),
            l.reason(Signal::Logs)
        ),
        (Some(4), true, "davy_spike")
    );
    assert!(!l.refuses(Signal::Sessions));
}
