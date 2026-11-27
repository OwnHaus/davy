//! The same logs stored hot and cold read the same: the hits, in order,
//! and the counts, for every query of a corpus.

use crate::logs::{
    LogLevel, LogRecord, LogSource, patterns,
    store::{
        LogQuery, State, histogram_all, histogram_for, no_overrides, search_all, search_for,
        variant,
    },
};
use crate::test_support::scratch;
use crate::time::{DAY_NS, day_of, midnight, now_ns};
use jiff::civil::Date;
use serde_json::{Value, json};
use std::{path::Path, time::Duration};

const MESSAGES: [&str; 10] = [
    "request {i} to upstream timed out",
    "GET /api/users/{i} 200 in 12ms",
    "TypeError: Cannot read properties of undefined (reading 'f{i}')",
    "BETA rollout reached step {i}",
    "Ünïcödé façade naïve café {i}",
    "payment {i} failed: card_declined",
    "cache refreshed after {i} misses",
    "user u-{i} signed in from 10.0.{i}.7",
    "disk usage at {i}% on /var/lib/davy",
    "",
];

fn attributes(i: i64) -> serde_json::Map<String, Value> {
    let v = match i % 6 {
        0 => json!({}),
        1 => json!({"req": {"id": format!("r-{i:08x}"), "method": "POST"}, "status": 500}),
        2 => json!({"res": {"statusCode": 200 + i % 3}, "tags": ["blue", "canary"], "ok": true}),
        3 => json!({"provider": "acme", "user": {"id": i, "name": "Zoë"}, "ratio": 0.25}),
        4 => json!({"nested": {"a": {"b": {"c": "deep value"}}}, "none": null}),
        _ => json!({"hostname": format!("api-{i:x}-0b1c"), "text": "say \"hi\"\n\ttab"}),
    };
    match v {
        Value::Object(m) => m,
        _ => unreachable!(),
    }
}

fn record(i: i64, timestamp: i64) -> LogRecord {
    let levels = [
        LogLevel::Trace,
        LogLevel::Debug,
        LogLevel::Info,
        LogLevel::Warn,
        LogLevel::Error,
        LogLevel::Fatal,
    ];
    let pick = usize::try_from(i).unwrap();
    LogRecord {
        project_id: 1 + i % 3,
        timestamp,
        observed_at: now_ns(),
        source: [LogSource::Otlp, LogSource::Logtail, LogSource::Vercel][pick % 3],
        level: levels[pick * 7 % 6],
        level_unknown: false,
        message: MESSAGES[pick % MESSAGES.len()].replace("{i}", &i.to_string()),
        service: (i % 4 != 0).then(|| ["api", "worker", "web"][pick % 3].into()),
        trace_id: (i % 5 == 0).then(|| format!("{:032x}", i * 7919)),
        span_id: (i % 5 == 0).then(|| format!("{:016x}", i * 31)),
        attributes: attributes(i),
    }
}

/// The days of the corpus, oldest first: one written before the `values`
/// field, one before patterns, and two of today's schema.
fn corpus_days() -> [Date; 4] {
    let back = |n: i64| day_of(now_ns() - n * DAY_NS);
    [back(12), back(11), back(10), back(9)]
}

/// 800 logs over the corpus days, each at its own time, a day after the
/// other: the writer keeps three days open.
fn batch(from: i64) -> Vec<LogRecord> {
    let days = corpus_days();
    let mut batch: Vec<LogRecord> = (from..from + 800)
        .map(|i| {
            let day = days[usize::try_from(i % 4).unwrap()];
            record(i, midnight(day) + (i * 97_000_000_013) % DAY_NS)
        })
        .collect();
    batch.sort_by_key(|r| day_of(r.timestamp));
    batch
}

fn write(dir: &Path, cold: bool) -> State {
    let days = corpus_days();
    for (day, values, pattern) in [(days[0], false, false), (days[1], true, false)] {
        let path = dir.join(day.to_string());
        std::fs::create_dir_all(&path).unwrap();
        tantivy::Index::create_in_dir(&path, variant(values, pattern).0).unwrap();
    }
    let mut state = State::new(dir.to_owned(), 30, no_overrides());
    state.add(batch(0)).unwrap();
    state.commit().unwrap();
    if cold {
        for day in days {
            state.turn_cold(day).unwrap();
        }
    }
    // Late logs: the two newest days are cold and hot both. Only those of
    // today's schema, as a late log for a cold day goes to a new index.
    let late = batch(800).into_iter().step_by(5);
    state
        .add(late.filter(|r| day_of(r.timestamp) >= days[2]).collect())
        .unwrap();
    state.commit().unwrap();
    state
}

const TEXTS: [&str; 36] = [
    "timed",
    "TIMED",
    "upstream AND timed",
    "\"to upstream timed out\"",
    "\"timed upstream\"",
    "timed OR declined",
    "payment -declined",
    "users NOT signed",
    "+cache +misses",
    "beta",
    "BETA rollout",
    "café",
    "naïve",
    "ünïcödé",
    "attributes.provider:acme",
    "attributes.req.method:post",
    "attributes.res.statusCode:201",
    "attributes.user.name:zoë",
    "attributes.nested.a.b.c:\"deep value\"",
    "0b1c",
    "canary",
    "500",
    "deep",
    "service:api",
    "level:5",
    "card_declined",
    "nothingmatchesthis",
    "users AND (200 OR 12ms) AND -signed",
    "*",
    "timed^3 OR declined",
    "\"to upstream ti\"*",
    "attributes.res.statusCode:[200 TO 299]",
    "attributes.res.statusCode:>=500",
    "level:[4 TO 6]",
    "service: IN [api worker]",
    "attributes.provider:*",
];

fn queries(trace: &str, pattern: u64) -> Vec<LogQuery> {
    let days = corpus_days();
    let all = LogQuery {
        project: None,
        since: 0,
        until: now_ns() + DAY_NS,
        min_level: None,
        text: None,
        pattern: None,
        limit: 1000,
        skip: 0,
    };
    let mut out = vec![all.clone()];
    let texts = TEXTS.iter().map(|t| (*t).to_owned());
    for text in texts.chain([format!("trace_id:{trace}"), trace.to_owned()]) {
        out.push(LogQuery {
            text: Some(text),
            ..all.clone()
        });
    }
    let base: Vec<LogQuery> = out.clone();
    for q in &base {
        out.push(LogQuery {
            project: Some(2),
            ..q.clone()
        });
        out.push(LogQuery {
            min_level: Some(LogLevel::Warn),
            ..q.clone()
        });
        out.push(LogQuery {
            limit: 7,
            ..q.clone()
        });
        out.push(LogQuery {
            limit: 1,
            ..q.clone()
        });
    }
    let day = midnight(days[2]);
    out.push(LogQuery {
        since: day + 3600,
        until: day + DAY_NS / 2,
        ..all.clone()
    });
    out.push(LogQuery {
        since: midnight(days[0]),
        until: midnight(days[1]) - 1,
        ..all.clone()
    });
    out.push(LogQuery {
        pattern: Some(pattern),
        ..all.clone()
    });
    out.push(LogQuery {
        pattern: Some(pattern),
        limit: 3,
        ..all.clone()
    });
    out
}

#[test]
fn the_same_logs_hot_and_cold_match_every_query_the_same() {
    let (hot, cold) = (scratch("diff-hot"), scratch("diff-cold"));
    let mut states = [write(&hot, false), write(&cold, true)];
    for (n, day) in corpus_days().into_iter().enumerate() {
        assert!(hot.join(day.to_string()).exists());
        assert_eq!(cold.join(day.to_string()).exists(), n >= 2, "late logs");
        assert_eq!(super::layout::segments(&cold, day).unwrap(), [1]);
    }
    let all = search_all(&hot, &queries("", 0)[0]).unwrap();
    assert_eq!(all.len(), 880);
    let trace = all.iter().find_map(|h| h.trace_id.clone()).unwrap();
    let pattern = all
        .iter()
        .find(|h| h.message.starts_with("payment") && h.pattern.is_some())
        .and_then(|h| patterns::parse(h.pattern.as_deref()?).ok())
        .unwrap();

    let mut corpus = queries(&trace, pattern);
    // A page after the first: the hit at `until` was on the page before.
    for at in [&all[100], &all[500], &all[879]] {
        let page = LogQuery {
            until: at.timestamp_ns,
            skip: 1,
            limit: 5,
            ..corpus[0].clone()
        };
        corpus.push(page);
    }
    assert_eq!(corpus.len(), 202);
    let (mut nonempty, mut refused) = (0, Vec::new());
    for q in corpus {
        // A query the day refuses, such as a range over an attribute, is
        // refused cold as well.
        let (h, c) = match (search_all(&hot, &q), search_all(&cold, &q)) {
            (Ok(h), Ok(c)) => (h, c),
            (Err(_), Err(_)) => {
                assert!(histogram_all(&cold, &q).is_err(), "{q:?}");
                if q.limit == 1000 && q.project.is_none() && q.min_level.is_none() {
                    refused.extend(q.text.clone());
                }
                continue;
            }
            (h, c) => panic!("{q:?}: hot {h:?}, cold {c:?}"),
        };
        assert_eq!(c, h, "{q:?}");
        nonempty += usize::from(!h.is_empty());
        let (h, c) = (
            histogram_all(&hot, &q).unwrap(),
            histogram_all(&cold, &q).unwrap(),
        );
        assert_eq!(
            (c.from, c.step, &c.counts),
            (h.from, h.step, &h.counts),
            "{q:?}"
        );
        assert!(h.cold.is_none());
        assert!(
            c.cold
                .is_some_and(|c| c.unscanned_to_ns.is_none() && c.corrupt_blocks == 0)
        );
    }
    assert_eq!(nonempty, 152);
    // Tantivy takes a range or `*` on a JSON path only over a fast field.
    assert_eq!(
        refused,
        [
            "attributes.res.statusCode:[200 TO 299]",
            "attributes.res.statusCode:>=500",
            "attributes.provider:*",
        ]
    );
    for s in &mut states {
        s.close_all().unwrap();
    }
    std::fs::remove_dir_all(&hot).unwrap();
    std::fs::remove_dir_all(&cold).unwrap();
}

/// A root with 40 logs of two projects ten days back, turned cold, and 10
/// of today, hot.
fn cold_and_hot(name: &str) -> (std::path::PathBuf, State, Date) {
    let dir = scratch(name);
    let day = day_of(now_ns() - 10 * DAY_NS);
    let mut state = State::new(dir.clone(), 30, no_overrides());
    let old = (0..40).map(|i| record(i, midnight(day) + i * 1_000_000));
    state.add(old.collect()).unwrap();
    state.commit().unwrap();
    state.turn_cold(day).unwrap();
    let today = (40..50).map(|i| record(i, now_ns() - 1_000_000 * (50 - i)));
    state.add(today.collect()).unwrap();
    state.commit().unwrap();
    (dir, state, day)
}

fn everything(limit: usize) -> LogQuery {
    LogQuery {
        project: None,
        since: 0,
        until: now_ns() + DAY_NS,
        min_level: None,
        text: None,
        pattern: None,
        limit,
        skip: 0,
    }
}

#[test]
fn a_search_the_hot_days_fill_opens_no_cold_file() {
    let (dir, mut state, _) = cold_and_hot("cold-newest-first");
    let found = search_for(&dir, &everything(10), Duration::MAX).unwrap();
    assert_eq!(found.hits.len(), 10);
    assert_eq!(found.cold, None);
    let found = search_for(&dir, &everything(11), Duration::MAX).unwrap();
    assert_eq!(found.hits.len(), 11);
    let cold = found.cold.unwrap();
    assert_eq!(cold.corrupt_files + cold.corrupt_blocks, 0);
    assert!(cold.rows > 0);
    state.close_all().unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_scan_past_its_budget_returns_what_it_found_and_what_it_did_not_read() {
    let (dir, mut state, day) = cold_and_hot("cold-budget");
    let found = search_for(&dir, &everything(100), Duration::ZERO).unwrap();
    assert_eq!(found.hits.len(), 10, "the hot ones");
    let cold = found.cold.unwrap();
    assert_eq!(cold.from_ns, midnight(day));
    assert_eq!(cold.to_ns, midnight(day) + DAY_NS - 1);
    assert_eq!(cold.rows, 0);
    assert_eq!(cold.unscanned_to_ns, Some(midnight(day) + 39 * 1_000_000));
    // A count takes a block whole from its header, but checks its bytes
    // first, so the budget stops it as well.
    let counts = histogram_for(&dir, &everything(0), Duration::ZERO).unwrap();
    assert_eq!(counts.counts.iter().sum::<u64>(), 10, "the hot ones");
    assert_eq!(
        counts.cold.unwrap().unscanned_to_ns,
        Some(midnight(day) + 39 * 1_000_000)
    );
    assert_eq!(
        histogram_all(&dir, &everything(0))
            .unwrap()
            .counts
            .iter()
            .sum::<u64>(),
        50
    );
    let mut timed = everything(0);
    timed.text = Some("timed".into());
    let counts = histogram_for(&dir, &timed, Duration::ZERO).unwrap();
    assert_eq!(counts.counts.iter().sum::<u64>(), 1, "log 40, hot");
    assert!(counts.cold.unwrap().unscanned_to_ns.is_some());
    assert_eq!(
        histogram_all(&dir, &timed)
            .unwrap()
            .counts
            .iter()
            .sum::<u64>(),
        5
    );
    assert_eq!(
        search_for(&dir, &everything(100), Duration::MAX)
            .unwrap()
            .hits
            .len(),
        50
    );
    state.close_all().unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_corrupt_block_or_file_is_left_out_and_counted() {
    let (dir, mut state, day) = cold_and_hot("cold-corrupt");
    let seg = super::layout::segment_dir(&dir, day, 1);
    let one = super::layout::file(&seg, 1);
    let mut bytes = std::fs::read(&one).unwrap();
    bytes[40] ^= 0xff;
    std::fs::write(&one, &bytes).unwrap();
    let found = search_for(&dir, &everything(100), Duration::MAX).unwrap();
    assert_eq!(found.hits.len(), 50 - 14);
    let cold = found.cold.unwrap();
    assert_eq!(
        (cold.corrupt_files, cold.corrupt_blocks, cold.corrupt_rows),
        (0, 1, 14)
    );
    // A count that takes blocks whole from their headers checks their
    // bytes, so it leaves out the same block as the search.
    let counts = histogram_all(&dir, &everything(0)).unwrap();
    assert_eq!(counts.counts.iter().sum::<u64>(), 50 - 14);
    assert_eq!(counts.cold.unwrap().corrupt_blocks, 1);
    let mut timed = everything(0);
    timed.text = Some("timed".into());
    let counts = histogram_all(&dir, &timed).unwrap();
    assert_eq!(counts.counts.iter().sum::<u64>(), 3, "logs 10, 20 and 40");
    assert_eq!(counts.cold.unwrap().corrupt_blocks, 1);

    let two = super::layout::file(&seg, 2);
    let len = std::fs::metadata(&two).unwrap().len();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&two)
        .unwrap()
        .set_len(len - 3)
        .unwrap();
    let found = search_for(&dir, &everything(100), Duration::MAX).unwrap();
    assert_eq!(found.hits.len(), 50 - 27);
    let cold = found.cold.unwrap();
    assert_eq!(
        (cold.corrupt_files, cold.corrupt_blocks, cold.corrupt_rows),
        (1, 1, 14)
    );
    state.close_all().unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_scan_cut_after_some_blocks_returns_only_logs_newer_than_the_cut() {
    let (dir, mut state, day) = cold_and_hot("cold-cut-later");
    // The blocks of projects 1, 3 and 2 end at logs 39, 38 and 37: the
    // budget lasts for the first.
    super::BLOCKS_LEFT.set(Some(1));
    let found = search_for(&dir, &everything(100), Duration::MAX).unwrap();
    super::BLOCKS_LEFT.set(None);
    let cut = midnight(day) + 38 * 1_000_000;
    assert_eq!(found.cold.unwrap().unscanned_to_ns, Some(cut));
    assert_eq!(found.hits.len(), 11, "the hot ones and log 39");
    assert!(found.hits.iter().all(|h| h.timestamp_ns > cut));

    super::BLOCKS_LEFT.set(Some(1));
    let counts = histogram_for(&dir, &everything(0), Duration::MAX).unwrap();
    super::BLOCKS_LEFT.set(None);
    assert_eq!(counts.cold.unwrap().unscanned_to_ns, Some(cut));
    assert_eq!(
        counts.counts.iter().sum::<u64>(),
        10 + 14,
        "the hot ones and project 1"
    );
    state.close_all().unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_pass_that_saw_a_day_hot_or_cold_reruns_once_it_changes_tier() {
    let dir = scratch("cold-parts-changed");
    let day = day_of(now_ns() - 10 * DAY_NS);
    let mut state = State::new(dir.clone(), 30, no_overrides());
    state
        .add((0..6).map(|i| record(i, midnight(day) + i)).collect())
        .unwrap();
    state.commit().unwrap();
    let hot = super::Parts::of(&dir, day).unwrap();
    assert!(hot.hot);
    hot.unchanged(&dir).unwrap();
    state.turn_cold(day).unwrap();
    let changed = hot.unchanged(&dir).unwrap_err();
    assert_eq!(changed.to_string(), super::CHANGED);

    let cold = super::Parts::of(&dir, day).unwrap();
    assert_eq!(cold.segments, [1]);
    cold.unchanged(&dir).unwrap();
    std::fs::remove_dir_all(super::layout::segment_dir(&dir, day, 1)).unwrap();
    let changed = cold.unchanged(&dir).unwrap_err();
    assert_eq!(changed.to_string(), super::CHANGED);
    state.close_all().unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_file_copied_from_another_day_is_left_out_and_counted() {
    let (dir, mut state, day) = cold_and_hot("cold-other-day");
    let other = day.yesterday().unwrap();
    let old = (0..40).map(|i| record(i, midnight(other) + i * 1_000_000));
    state.add(old.collect()).unwrap();
    state.commit().unwrap();
    state.turn_cold(other).unwrap();
    let from = super::layout::segment_dir(&dir, day, 1);
    let to = super::layout::segment_dir(&dir, other, 1);
    std::fs::copy(super::layout::file(&from, 1), super::layout::file(&to, 1)).unwrap();
    let found = search_for(&dir, &everything(1000), Duration::MAX).unwrap();
    assert_eq!(
        found.hits.len(),
        50 + 40 - 14,
        "project 1 of the day before is left out"
    );
    let cold = found.cold.unwrap();
    assert_eq!((cold.corrupt_files, cold.corrupt_blocks), (1, 0));
    let counts = histogram_all(&dir, &everything(0)).unwrap();
    assert_eq!(counts.counts.iter().sum::<u64>(), 50 + 40 - 14);
    state.close_all().unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_file_under_another_projects_name_is_left_out_and_counted() {
    let (dir, mut state, day) = cold_and_hot("cold-other-project");
    let segment = super::layout::segment_dir(&dir, day, 1);
    let to = super::layout::file(&segment, 9);
    std::fs::copy(super::layout::file(&segment, 1), &to).unwrap();
    let found = search_for(&dir, &everything(1000), Duration::MAX).unwrap();
    assert_eq!(found.hits.len(), 50, "the copy is left out");
    let cold = found.cold.unwrap();
    assert_eq!((cold.corrupt_files, cold.corrupt_blocks), (1, 0));
    let nine = LogQuery {
        project: Some(9),
        ..everything(1000)
    };
    assert_eq!(
        search_for(&dir, &nine, Duration::MAX).unwrap().hits.len(),
        0
    );
    let counts = histogram_all(&dir, &everything(0)).unwrap();
    assert_eq!(counts.counts.iter().sum::<u64>(), 50);
    state.close_all().unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Logs of the last day before 1970, cold, counted in buckets that start
/// at the last nanosecond there is: `ts - from` overflows for each.
#[test]
fn a_count_whose_bucket_overflows_counts_nothing() {
    let dir = scratch("cold-bucket-overflow");
    let day: Date = "1969-12-31".parse().unwrap();
    let mut state = State::new(dir.clone(), 36_500, no_overrides());
    let old = (0..40).map(|i| record(i, midnight(day) + i * 1_000_000));
    state.add(old.collect()).unwrap();
    state.commit().unwrap();
    state.turn_cold(day).unwrap();
    let q = LogQuery {
        since: i64::MIN,
        until: i64::MAX,
        ..everything(0)
    };
    let parts = super::Parts::of(&dir, day).unwrap();
    assert_eq!(parts.segments, [1]);
    let mut counts = [0; 4];
    super::Scan::new(&q, Duration::MAX)
        .count(&dir, &parts, (i64::MAX, 1 << 62), &mut counts)
        .unwrap();
    assert_eq!(counts, [0; 4]);
    // The same logs, counted from before them, are all there.
    super::Scan::new(&q, Duration::MAX)
        .count(&dir, &parts, (midnight(day), DAY_NS), &mut counts)
        .unwrap();
    assert_eq!(counts, [40, 0, 0, 0]);
    state.close_all().unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}
