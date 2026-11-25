//! What drop rules cost the log writer per line: `Judge::keep` alone,
//! over lines made before the clock starts, fastest of 7 rounds.
//!
//! cargo run --release --example drop_judge
//!
//! Each case prints nanoseconds per line. Rules that never match are what
//! a rule costs at most on a line that is stored: every one is tried. A
//! rule that matches also counts the line's raw bytes, and a pattern rule
//! works out the line's pattern, which the pattern counters would
//! otherwise work out for a stored line.

use anyhow::Result;
use davy::{
    drops::{By, Judge, Live, Match, Seen, Set, Spec, Stored},
    logs::{LogLevel, LogRecord, LogSource, patterns},
};
use std::{hint::black_box, io::Write, time::Instant};

const LINES: usize = 20_000;
const ROUNDS: usize = 7;

/// An access line of a made-up API, about 90 bytes.
fn short(i: usize) -> String {
    let route = ["/v1/events", "/v1/orders", "/health", "/v1/search"][i % 4];
    format!(
        "GET {route}?page={} 200 in {}ms request_id={:08x}",
        i % 50,
        i % 900,
        i.wrapping_mul(2_654_435_761)
    )
}

/// A 64 KiB message, the most a line holds: words of a JSON body.
fn long(i: usize) -> String {
    let mut s = format!("payload {i}: ");
    let words = [
        "\"order\":",
        "{\"id\":",
        "12345,",
        "\"items\":[",
        "\"sku-",
        "\"qty\":3}",
        "],",
    ];
    let mut n = i;
    while s.len() < 64 * 1024 - 16 {
        n = n.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        s.push_str(words[(n >> 33) % words.len()]);
    }
    s
}

/// A 64 KiB message that starts a rule's text at every few bytes and
/// never holds it, the most a message rule can cost.
fn near_misses(i: usize) -> String {
    let mut s = format!("{i} ");
    while s.len() < 64 * 1024 - 16 {
        s.push_str("no line holds x ");
    }
    s
}

fn line(message: String) -> LogRecord {
    LogRecord {
        project_id: 1,
        timestamp: 1_795_600_800_000_000_000,
        observed_at: 1_795_600_800_000_000_000,
        source: LogSource::Otlp,
        level: LogLevel::Info,
        level_unknown: false,
        message,
        service: Some("api".into()),
        trace_id: None,
        span_id: None,
        attributes: serde_json::Map::new(),
    }
}

fn rule(id: i64, live: bool, matches: Match, keep: u8) -> Stored {
    Stored {
        id,
        project: 1,
        position: id,
        live,
        by: By::Person,
        spec: Spec {
            matches,
            service: None,
            keep,
            any_level: false,
        },
        created_at: 0,
        changed_at: 0,
        expires_at: None,
    }
}

/// `n` shadow message rules that no line holds, `*` in each with `glob`.
fn never(n: usize, glob: bool) -> Vec<Stored> {
    (0..n)
        .map(|i| {
            let text = if glob {
                format!("no line*holds {i}")
            } else {
                format!("no line holds {i}")
            };
            rule(
                i64::try_from(i).unwrap_or(0) + 1,
                false,
                Match::Message(text),
                0,
            )
        })
        .collect()
}

/// Nanoseconds per line of the fastest round of [`ROUNDS`].
fn time(rules: &[Stored], lines: &[LogRecord]) -> f64 {
    let live = Live::default();
    live.set(Set::compile(rules));
    let mut judge = Judge::new(live);
    judge.refresh();
    let mut best = f64::MAX;
    for _ in 0..ROUNDS {
        let started = Instant::now();
        for r in lines {
            black_box(judge.keep(black_box(r), &mut Seen::default()));
        }
        best = best.min(started.elapsed().as_secs_f64());
        black_box(judge.take());
    }
    #[expect(clippy::cast_precision_loss, reason = "a count of lines to 3 digits")]
    let per = best * 1e9 / lines.len() as f64;
    per
}

fn main() -> Result<()> {
    let shorts: Vec<LogRecord> = (0..LINES).map(|i| line(short(i))).collect();
    let longs: Vec<LogRecord> = (0..LINES / 20).map(|i| line(long(i))).collect();
    let misses: Vec<LogRecord> = (0..LINES / 20).map(|i| line(near_misses(i))).collect();
    let id = patterns::of(&shorts[0].message).id;
    let cases: [(&str, Vec<Stored>, &[LogRecord]); 12] = [
        ("no rule", vec![], &shorts),
        ("10 message rules, none match", never(10, false), &shorts),
        ("50 message rules, none match", never(50, false), &shorts),
        ("50 globs, none match", never(50, true), &shorts),
        (
            "1 message rule that matches, keep 1%",
            vec![rule(1, true, Match::Message("200 in".into()), 1)],
            &shorts,
        ),
        (
            "1 glob that matches, drop all",
            vec![rule(1, true, Match::Message("GET *ms*".into()), 0)],
            &shorts,
        ),
        (
            "1 pattern rule, drops a quarter",
            vec![rule(1, true, Match::Pattern(id), 0)],
            &shorts,
        ),
        ("no rule, 64 KiB messages", vec![], &longs),
        (
            "10 message rules, none match, 64 KiB messages",
            never(10, false),
            &longs,
        ),
        (
            "50 message rules, none match, 64 KiB messages",
            never(50, false),
            &longs,
        ),
        (
            "50 message rules, 64 KiB of near misses",
            never(50, false),
            &misses,
        ),
        ("50 globs, 64 KiB of near misses", never(50, true), &misses),
    ];
    let mut out = std::io::stdout().lock();
    for (name, rules, lines) in cases {
        writeln!(out, "{:>10.1} ns per line  {name}", time(&rules, lines))?;
    }
    Ok(())
}
