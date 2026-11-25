use super::super::{By, Match, Spec, Stored};
use super::*;
use crate::logs::LogSource;

/// 2026-11-25T10:00:00Z in nanoseconds.
const NOON_NS: i64 = 1_795_600_800_000_000_000;

fn line(project_id: i64, level: LogLevel, service: Option<&str>, message: &str) -> LogRecord {
    LogRecord {
        project_id,
        timestamp: NOON_NS,
        observed_at: NOON_NS,
        source: LogSource::Otlp,
        level,
        level_unknown: false,
        message: message.into(),
        service: service.map(Into::into),
        trace_id: None,
        span_id: None,
        attributes: serde_json::Map::new(),
    }
}

fn info(message: &str) -> LogRecord {
    line(1, LogLevel::Info, None, message)
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

fn judge(rules: &[Stored]) -> Judge {
    let live = Live::default();
    live.set(Set::compile(rules));
    let mut j = Judge::new(live);
    j.refresh();
    j
}

fn kept(j: &mut Judge, r: &LogRecord) -> bool {
    j.keep(r, &mut Seen::default())
}

fn key(rule: i64, live: bool) -> Key {
    Key {
        day: jiff::civil::date(2026, 11, 25),
        rule,
        live,
        project: 1,
    }
}

fn message(text: &str) -> Match {
    Match::Message(text.into())
}

#[test]
fn the_first_live_rule_that_matches_decides() {
    let mut j = judge(&[
        rule(1, true, message("health"), 50),
        rule(2, true, message("check"), 0),
    ]);
    // Rule 1 keeps the first of every two; rule 2 never sees them.
    let results: Vec<bool> = (0..4)
        .map(|_| kept(&mut j, &info("health check")))
        .collect();
    assert_eq!(results, [true, false, true, false]);
    assert!(!kept(&mut j, &info("check only")));
    let bytes = |m: &str| crate::logs::store::raw_bytes(&info(m));
    assert_eq!(
        j.take(),
        HashMap::from([
            (
                key(1, true),
                Count {
                    records: 2,
                    bytes: 2 * bytes("health check")
                }
            ),
            (
                key(2, true),
                Count {
                    records: 1,
                    bytes: bytes("check only")
                }
            ),
        ])
    );
    assert_eq!(j.take(), HashMap::new(), "take empties the counts");
}

#[test]
fn keep_1_percent_keeps_exactly_10_of_1000_lines() {
    let mut j = judge(&[rule(1, true, message("poll"), 1)]);
    let kept_at: Vec<usize> = (0..1000).filter(|_| kept(&mut j, &info("poll"))).collect();
    assert_eq!(kept_at, [0, 100, 200, 300, 400, 500, 600, 700, 800, 900]);
    assert_eq!(j.take()[&key(1, true)].records, 990);
}

#[test]
fn keep_33_percent_keeps_exactly_330_of_1000_lines() {
    let mut j = judge(&[rule(1, true, message("poll"), 33)]);
    let n = (0..1000).filter(|_| kept(&mut j, &info("poll"))).count();
    assert_eq!(n, 330);
}

#[test]
fn a_shadow_rule_drops_nothing_and_counts_what_it_would_drop() {
    let mut j = judge(&[rule(1, false, message("poll"), 10)]);
    let n = (0..1000).filter(|_| kept(&mut j, &info("poll"))).count();
    assert_eq!(n, 1000);
    assert_eq!(j.take()[&key(1, false)].records, 900);
}

#[test]
fn a_shadow_rule_counts_only_lines_no_live_rule_matched() {
    let mut j = judge(&[
        rule(1, true, message("debug"), 0),
        rule(2, false, message("poll"), 0),
        rule(3, false, message("poll"), 0),
    ]);
    assert!(!kept(&mut j, &info("debug poll")));
    assert!(kept(&mut j, &info("poll")));
    let mut counted: Vec<(i64, bool, u64)> = j
        .take()
        .into_iter()
        .map(|(k, n)| (k.rule, k.live, n.records))
        .collect();
    counted.sort_unstable();
    // Each shadow rule counts on its own; the live rule took the first line.
    assert_eq!(counted, [(1, true, 1), (2, false, 1), (3, false, 1)]);
}

#[test]
fn a_level_rule_matches_that_level_and_every_lower_one() {
    let mut j = judge(&[rule(1, true, Match::Level(LogLevel::Debug), 0)]);
    let levels = [
        LogLevel::Trace,
        LogLevel::Debug,
        LogLevel::Info,
        LogLevel::Warn,
    ];
    let stored = levels.map(|l| kept(&mut j, &line(1, l, None, "x")));
    assert_eq!(stored, [false, false, true, true]);
}

#[test]
fn a_rule_with_a_service_matches_that_service_only() {
    let mut r = rule(1, true, message("poll"), 0);
    r.spec.service = Some("api".into());
    let mut j = judge(&[r]);
    let stored = [Some("api"), Some("api2"), None]
        .map(|s| kept(&mut j, &line(1, LogLevel::Info, s, "poll")));
    assert_eq!(stored, [false, true, true]);
}

#[test]
fn a_message_with_a_star_matches_the_whole_message() {
    let mut j = judge(&[rule(1, true, message("GET /health* 200*ms"), 0)]);
    let stored = [
        "GET /health 200 in 3ms",
        "GET /healthz?x=1 200 12ms",
        "x GET /health 200 3ms",
        "GET /health 500 3ms",
        "GET /health 200 3ms!",
    ]
    .map(|m| kept(&mut j, &info(m)));
    assert_eq!(stored, [false, false, true, true, true]);
}

#[test]
fn a_glob_does_not_let_its_start_and_end_overlap() {
    let mut j = judge(&[rule(1, true, message("ab*b"), 0)]);
    assert_eq!(
        ["ab", "abb", "abxb"].map(|m| kept(&mut j, &info(m))),
        [true, false, false]
    );
}

#[test]
fn a_pattern_rule_matches_the_lines_of_its_pattern_and_tells_the_pattern() {
    let id = patterns::of("job 17 done in 3ms").id;
    let mut j = judge(&[rule(1, false, Match::Pattern(id), 0)]);
    let mut seen = Seen::default();
    assert!(j.keep(&info("job 18 done in 5ms"), &mut seen));
    assert_eq!(seen.take_pattern().map(|p| p.id), Some(id));
    assert!(j.keep(&info("other text"), &mut Seen::default()));
    assert_eq!(j.take()[&key(1, false)].records, 1);
}

#[test]
fn a_project_without_rules_works_nothing_out() {
    let mut j = judge(&[rule(1, true, message("poll"), 0)]);
    let mut seen = Seen::default();
    assert!(j.keep(&line(2, LogLevel::Info, None, "poll"), &mut seen));
    assert_eq!(seen.take_pattern(), None);
    assert_eq!(j.take(), HashMap::new());
}

#[test]
fn a_changed_rule_starts_its_keep_counter_over() {
    let live = Live::default();
    let mut r = rule(1, true, message("poll"), 50);
    live.set(Set::compile(std::slice::from_ref(&r)));
    let mut j = Judge::new(live.clone());
    j.refresh();
    assert!(kept(&mut j, &info("poll")), "the first line is kept");
    // The same rule again keeps its count, so the second line is dropped;
    // a change starts it over.
    live.set(Set::compile(std::slice::from_ref(&r)));
    j.refresh();
    assert!(!kept(&mut j, &info("poll")), "a reload keeps the count");
    r.changed_at = 1;
    live.set(Set::compile(std::slice::from_ref(&r)));
    j.refresh();
    assert!(
        kept(&mut j, &info("poll")),
        "the first line after a change is kept"
    );
}

#[test]
fn a_set_holds_at_most_50_rules_of_a_project() {
    let rules: Vec<Stored> = (1..=60)
        .map(|i| rule(i, true, message(&format!("line {i} ")), 0))
        .collect();
    let mut j = judge(&rules);
    assert!(!kept(&mut j, &info("line 50 ")));
    assert!(kept(&mut j, &info("line 51 ")));
}

#[test]
fn a_pattern_rule_skips_warnings_and_worse_unless_at_any_level() {
    let id = patterns::of("job 17 done in 3ms").id;
    let mut any = rule(2, true, Match::Pattern(id), 0);
    any.spec.any_level = true;
    let levels = [
        LogLevel::Info,
        LogLevel::Warn,
        LogLevel::Error,
        LogLevel::Fatal,
    ];
    let mut below = judge(&[rule(1, true, Match::Pattern(id), 0)]);
    let stored = levels.map(|l| kept(&mut below, &line(1, l, None, "job 18 done in 5ms")));
    assert_eq!(stored, [false, true, true, true]);
    let mut at_any = judge(&[any]);
    let stored = levels.map(|l| kept(&mut at_any, &line(1, l, None, "job 18 done in 5ms")));
    assert_eq!(stored, [false, false, false, false]);
}

#[test]
fn a_level_rule_never_drops_a_line_whose_level_was_not_understood() {
    // The spellings a review sent through `level`, and four that are info
    // or below. `--level info` drops those four only.
    let levels = [
        r#""WARNING""#,
        r#""Warn""#,
        r#""ERR""#,
        r#""Error""#,
        r#""critical""#,
        r#""warn ""#,
        r#""ERROR ""#,
        "40",
        "50",
        r#""severe""#,
        r#""SEVERE""#,
        r#""E""#,
        r#""W""#,
        r#""fault""#,
        r#""failure""#,
        r#""exception""#,
        r#""err0r""#,
        r#""Level.ERROR""#,
        r#""error:""#,
        r#""[error]""#,
        r#""40""#,
        r#""50""#,
        r#""""#,
        "missing",
        r#""info""#,
        r#""debug""#,
        r#""trace""#,
        r#""notice""#,
    ];
    let body: Vec<String> = levels
        .iter()
        .map(|l| match *l {
            "missing" => r#"{"message":"x"}"#.to_owned(),
            l => format!(r#"{{"message":"x","level":{l}}}"#),
        })
        .collect();
    let body = format!("[{}]", body.join(","));
    let lines =
        crate::logs::logtail::parse(body.as_bytes(), "application/json", 1, NOON_NS).unwrap();
    let mut j = judge(&[rule(1, true, Match::Level(LogLevel::Info), 0)]);
    let dropped: Vec<&str> = lines
        .iter()
        .zip(levels)
        .filter(|(r, _)| !kept(&mut j, r))
        .map(|(_, l)| l)
        .collect();
    assert_eq!(
        dropped,
        [r#""info""#, r#""debug""#, r#""trace""#, r#""notice""#]
    );
    let unknown: Vec<&str> = lines
        .iter()
        .zip(levels)
        .filter(|(r, _)| r.level_unknown)
        .map(|(_, l)| l)
        .collect();
    assert_eq!(unknown, [r#""err0r""#, r#""""#, "missing"]);
}
