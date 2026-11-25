use super::*;
use crate::usage::{Count, Counter, Signal};
use limit::Key;
use std::thread;

/// 2026-01-01T00:00:00Z.
const AT: i64 = 1_767_225_600_000_000_000;

fn key(n: u64) -> Key {
    ("test", n)
}

const RECORD: Admit = Admit::Record { suppressed: 0 };

#[test]
fn a_pattern_is_recorded_ten_times_an_hour_and_then_counted() {
    assert_eq!((PER_WINDOW, WINDOW_NS), (10, 3_600_000_000_000));
    let mut l = Limiter::default();
    for i in 0..10 {
        assert_eq!(l.admit(key(1), AT + i), RECORD);
    }
    assert_eq!(l.admit(key(1), AT + 10), Admit::Count);
    assert_eq!(l.admit(key(1), AT + 11), Admit::Count);
    assert_eq!(l.admit(key(2), AT + 12), RECORD, "a window of its own");
    assert_eq!(
        l.admit(key(1), AT + WINDOW_NS),
        Admit::Record { suppressed: 2 }
    );
    assert_eq!(l.admit(key(1), AT + WINDOW_NS + 1), RECORD);
}

#[test]
fn a_clock_that_went_back_keeps_the_window() {
    let mut l = Limiter::default();
    for _ in 0..10 {
        l.admit(key(1), AT);
    }
    assert_eq!(l.admit(key(1), AT - WINDOW_NS), Admit::Count);
}

#[test]
fn past_max_keys_new_patterns_share_one_window_until_a_window_ends() {
    assert_eq!(MAX_KEYS, 1024);
    let mut l = Limiter::default();
    for n in 0..1024 {
        assert_eq!(l.admit(key(n), AT), RECORD);
    }
    for n in 0..10 {
        assert_eq!(
            l.admit(key(5000 + n), AT + 1),
            RECORD,
            "the overflow window"
        );
    }
    assert_eq!(l.admit(key(6000), AT + 1), Admit::Count);
    assert_eq!(l.admit(key(0), AT + 1), RECORD, "a counted pattern goes on");
    assert_eq!(l.take_lost(), 0);
    assert_eq!(l.admit(key(5000), AT + WINDOW_NS), RECORD);
}

#[test]
fn a_count_of_a_forgotten_window_is_not_lost() {
    let mut l = Limiter::default();
    for _ in 0..12 {
        l.admit(key(0), AT);
    }
    for n in 1..1024 {
        l.admit(key(n), AT + WINDOW_NS - 1);
    }
    // Key 0's window ended with 2 lines counted. A new key forgets it.
    assert_eq!(l.admit(key(5000), AT + WINDOW_NS), RECORD);
    assert_eq!(l.take_lost(), 2);
    assert_eq!(l.take_lost(), 0);
}

#[test]
fn a_dropped_line_counts_what_it_carried() {
    let (sink, lines) = Sink::new(1);
    for i in 0..12 {
        sink.offer(LogLevel::Error, "ui", &format!("job {i} failed"), AT + i);
    }
    // The queue holds job 0, jobs 1 to 9 were dropped and 10 and 11
    // counted. The next line of the pattern carries both counts, and is
    // dropped too.
    assert_eq!(
        sink.offer(LogLevel::Error, "ui", "job 99 failed", AT + WINDOW_NS),
        Offer::Dropped
    );
    assert_eq!(lines.try_recv().unwrap().message, "job 0 failed");
    assert_eq!(
        sink.offer(LogLevel::Error, "z", "next", AT + WINDOW_NS),
        Offer::Queued
    );
    assert_eq!(lines.try_recv().unwrap().dropped, 9 + 2 + 1);
}

#[test]
fn a_line_offered_is_queued_with_its_level_component_and_message() {
    let (sink, lines) = Sink::new(4);
    let offered = sink.offer(LogLevel::Warn, "sweep", "log_days not pruned", AT);
    assert_eq!(offered, Offer::Queued);
    assert_eq!(
        lines.try_recv().unwrap(),
        Line {
            at_ns: AT,
            level: LogLevel::Warn,
            component: "sweep",
            message: "log_days not pruned".to_owned(),
            suppressed: 0,
            dropped: 0,
        }
    );
}

#[test]
fn lines_past_the_limit_are_counted_in_the_next_one_recorded() {
    let (sink, lines) = Sink::new(32);
    let offers: Vec<Offer> = (0..13)
        .map(|i| sink.offer(LogLevel::Error, "ui", &format!("job {i} failed"), AT + i))
        .collect();
    assert_eq!(offers[..10], [const { Offer::Queued }; 10]);
    assert_eq!(
        offers[10..],
        [Offer::Limited, Offer::Limited, Offer::Limited]
    );
    assert_eq!(lines.try_iter().count(), 10);
    let next = sink.offer(LogLevel::Error, "ui", "job 99 failed", AT + WINDOW_NS);
    assert_eq!(next, Offer::Queued);
    let line = lines.try_recv().unwrap();
    assert_eq!(
        (line.message.as_str(), line.suppressed),
        ("job 99 failed", 3)
    );
}

#[test]
fn a_full_queue_drops_lines_and_the_next_one_queued_counts_them() {
    let (sink, lines) = Sink::new(1);
    assert_eq!(sink.offer(LogLevel::Error, "a", "first", AT), Offer::Queued);
    assert_eq!(
        sink.offer(LogLevel::Error, "b", "second", AT),
        Offer::Dropped
    );
    assert_eq!(
        sink.offer(LogLevel::Error, "c", "third", AT),
        Offer::Dropped
    );
    assert_eq!(lines.try_recv().unwrap().message, "first");
    assert_eq!(
        sink.offer(LogLevel::Error, "d", "fourth", AT),
        Offer::Queued
    );
    let line = lines.try_recv().unwrap();
    assert_eq!((line.message.as_str(), line.dropped), ("fourth", 2));
    assert_eq!(sink.offer(LogLevel::Error, "e", "fifth", AT), Offer::Queued);
    assert_eq!(lines.try_recv().unwrap().dropped, 0);
}

#[test]
fn a_line_for_a_forwarder_that_is_gone_is_closed() {
    let (sink, lines) = Sink::new(1);
    drop(lines);
    assert_eq!(sink.offer(LogLevel::Warn, "a", "x", AT), Offer::Closed);
}

#[test]
fn a_sink_whose_forwarder_is_gone_is_uninstalled() {
    let _lock = install_lock();
    let (sink, lines) = Sink::new(4);
    let _installed = install(&sink);
    drop(lines);
    warn("closed test", "lost");
    assert!(installed().is_none());
}

#[test]
fn a_long_message_is_capped_at_a_character_boundary() {
    assert_eq!(capped("héllo", 2), "h");
    assert_eq!(capped("héllo", 3), "hé");
    assert_eq!(capped("abc", 10), "abc");
    let (sink, lines) = Sink::new(1);
    sink.offer(LogLevel::Warn, "ui", &"x".repeat(MAX_MESSAGE + 5), AT);
    assert_eq!(lines.try_recv().unwrap().message.len(), 8192);
}

/// The lines of `component`, as other tests may report while a sink is
/// installed.
fn of(lines: &mpsc::Receiver<Line>, component: &str) -> Vec<(LogLevel, String)> {
    lines
        .try_iter()
        .filter(|l| l.component == component)
        .map(|l| (l.level, l.message))
        .collect()
}

#[test]
fn lines_reach_the_installed_sink_and_stop_when_it_is_gone() {
    let _lock = install_lock();
    let (sink, lines) = Sink::new(64);
    let installed = install(&sink);
    info("routing test", "one");
    warn("routing test", "two");
    error("routing test", 3);
    log(LogLevel::Fatal, "routing test", "four");
    assert_eq!(
        of(&lines, "routing test"),
        [
            (LogLevel::Info, "one".to_owned()),
            (LogLevel::Warn, "two".to_owned()),
            (LogLevel::Error, "3".to_owned()),
            (LogLevel::Fatal, "four".to_owned()),
        ]
    );
    drop(installed);
    warn("routing test", "after");
    assert_eq!(of(&lines, "routing test"), []);
}

#[test]
fn a_line_while_quiet_goes_to_stderr_only() {
    let _lock = install_lock();
    let (sink, lines) = Sink::new(64);
    let _installed = install(&sink);
    {
        let _outer = quiet();
        {
            let _inner = quiet();
            warn("quiet test", "inner");
        }
        warn("quiet test", "outer");
        let other = thread::spawn(|| warn("quiet test", "other thread"));
        other.join().unwrap();
    }
    warn("quiet test", "loud");
    assert_eq!(
        of(&lines, "quiet test"),
        [
            (LogLevel::Warn, "other thread".to_owned()),
            (LogLevel::Warn, "loud".to_owned()),
        ]
    );
}

#[test]
fn an_older_guard_leaves_a_newer_sink_installed() {
    let _lock = install_lock();
    let (old, _old_lines) = Sink::new(1);
    let (new, lines) = Sink::new(8);
    let first = install(&old);
    let second = install(&new);
    drop(first);
    warn("guard test", "kept");
    assert_eq!(
        of(&lines, "guard test"),
        [(LogLevel::Warn, "kept".to_owned())]
    );
    drop(second);
    assert!(installed().is_none());
}

#[test]
fn counts_held_of_one_project_alone_are_its_own() {
    let counter = Counter::default();
    assert!(!counter.only(7), "nothing held");
    counter.add(7, Signal::Logs, Count::new(1, 10));
    assert!(counter.only(7));
    counter.add(8, Signal::Errors, Count::new(1, 10));
    assert!(!counter.only(7));
}
