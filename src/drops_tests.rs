use super::*;
use crate::{store, usage::Count};
use jiff::civil::{Date, date};
use std::collections::HashMap;

/// 2026-11-25T10:00:00Z in milliseconds.
const NOON_MS: i64 = 1_795_600_800_000;

fn memory() -> Connection {
    let mut c = Connection::open_in_memory().unwrap();
    store::init(&mut c).unwrap();
    store::create_project(&c, "shop").unwrap();
    store::create_project(&c, "blog").unwrap();
    c
}

fn message(text: &str) -> Spec {
    Spec::new(None, None, Some(text), None, None).unwrap()
}

fn added(c: &Connection, spec: &Spec, live: bool, by: By) -> Result<Added, String> {
    add(c, 1, spec, live, by, NOON_MS).unwrap()
}

fn ids(c: &Connection, at_ms: i64) -> Vec<(i64, bool)> {
    load(c, Some(1), at_ms)
        .unwrap()
        .into_iter()
        .map(|r| (r.id, r.live))
        .collect()
}

#[test]
fn a_rule_names_exactly_one_of_pattern_level_and_message() {
    let spec = |p, l, m| Spec::new(p, l, m, None, None);
    let one = "name exactly one of pattern, level and message";
    assert_eq!(spec(None, None, None), Err(one.into()));
    assert_eq!(spec(None, Some("debug"), Some("x")), Err(one.into()));
    assert_eq!(
        spec(Some("other"), None, None),
        Err("pattern other is no pattern: it counts many kinds of lines".into())
    );
    assert_eq!(spec(None, None, Some("")), Err("message is empty".into()));
    assert_eq!(
        spec(None, None, Some("**")),
        Err("message is only *, which matches every line: use --level, or name the text".into())
    );
    assert_eq!(
        Spec::new(None, Some("debug"), None, Some("*"), None),
        Err("service is only *: a service is matched exactly, and * is no wildcard in it; leave --service out for every service".into())
    );
    let long = "x".repeat(MAX_TEXT + 1);
    assert_eq!(
        spec(None, None, Some(&long)),
        Err("message is longer than 256 bytes".into())
    );
    assert_eq!(
        Spec::new(None, Some("debug"), None, Some("api"), Some("5%")),
        Ok(Spec {
            matches: Match::Level(LogLevel::Debug),
            service: Some("api".into()),
            keep: 5,
            any_level: false,
        })
    );
}

#[test]
fn rule_text_holds_no_character_a_person_cannot_see() {
    for (text, code) in [
        ("poll\u{1b}[2J", "001B"),
        ("ok\u{200B}", "200B"),
        ("a\u{00A0}b", "00A0"),
        ("a\u{2028}b", "2028"),
        ("a\u{2029}b", "2029"),
        ("\u{FEFF}ok", "FEFF"),
        ("o\u{2060}k", "2060"),
        ("\u{202E}ko", "202E"),
        ("\u{00AD}ok", "00AD"),
        ("ok\u{E0041}", "E0041"),
    ] {
        let refused = format!(
            "message holds U+{code}, which a person cannot see or tell from a space: type it without"
        );
        assert_eq!(Spec::new(None, None, Some(text), None, None), Err(refused));
    }
    assert_eq!(
        Spec::new(None, Some("info"), None, Some("a\u{200F}"), None),
        Err(
            "service holds U+200F, which a person cannot see or tell from a space: type it without"
                .into()
        )
    );
    assert!(Spec::new(None, None, Some("café ✓ 日本"), None, None).is_ok());
}

#[test]
fn keep_is_a_percent_from_1_to_99() {
    assert_eq!(parse_keep("1%"), Ok(1));
    assert_eq!(parse_keep("99"), Ok(99));
    let refused =
        |s: &str| format!("keep {s:?} is not a percent from 1 to 99; leave it out to drop all");
    for s in ["0%", "100%", "50.5%", "-1", "%", "+5", "+5%", " 5 %"] {
        assert_eq!(parse_keep(s), Err(refused(s)));
    }
}

#[test]
fn options_quote_text_for_a_shell() {
    let spec = Spec::new(None, None, Some("it's done"), Some("a b"), Some("10")).unwrap();
    assert_eq!(
        spec.options(),
        r"--message 'it'\''s done' --service 'a b' --keep 10%"
    );
}

#[test]
fn rules_go_after_the_others_and_an_equal_rule_is_found_not_added() {
    let c = memory();
    let first = added(&c, &message("health"), true, By::Person).unwrap();
    let second = added(&c, &message("poll"), false, By::Person).unwrap();
    assert_eq!((first.id, second.id), (1, 2));
    assert_eq!(
        added(&c, &message("health"), true, By::Person),
        Ok(Added {
            id: 1,
            created: false,
            taken_over: false
        })
    );
    assert_eq!(ids(&c, NOON_MS), [(1, true), (2, false)]);
}

#[test]
fn an_agent_cannot_add_a_live_rule() {
    let c = memory();
    assert_eq!(
        added(&c, &message("poll"), true, By::Agent),
        Err("an agent adds shadow rules only: a person makes a rule live".into())
    );
    assert_eq!(ids(&c, NOON_MS), []);
}

#[test]
fn an_agent_holds_at_most_10_shadow_rules_in_a_project() {
    let c = memory();
    for i in 0..MAX_AGENT_SHADOW {
        added(&c, &message(&format!("line {i}")), false, By::Agent).unwrap();
    }
    assert_eq!(
        added(&c, &message("line 10"), false, By::Agent),
        Err(
            "an agent holds 10 shadow rules in this project, the most it may: remove one first"
                .into()
        )
    );
    // A person's rules and another project's do not count against it.
    assert!(added(&c, &message("line 10"), false, By::Person).is_ok());
    let other = add(&c, 2, &message("line 10"), false, By::Agent, NOON_MS).unwrap();
    assert!(other.is_ok());
}

#[test]
fn a_project_holds_at_most_50_rules() {
    let c = memory();
    for i in 0..MAX_RULES {
        added(&c, &message(&format!("line {i}")), true, By::Person).unwrap();
    }
    assert_eq!(
        added(&c, &message("line 50"), true, By::Person),
        Err("the project has 50 rules, the most it may hold: remove one first".into())
    );
}

#[test]
fn an_agents_shadow_rule_expires_after_14_days() {
    let c = memory();
    let rule = added(&c, &message("poll"), false, By::Agent).unwrap();
    let expires = NOON_MS + 14 * DAY_MS;
    assert_eq!(get(&c, rule.id).unwrap().unwrap().expires_at, Some(expires));
    assert_eq!(ids(&c, expires - 1), [(1, false)]);
    assert_eq!(ids(&c, expires), []);
    // An expired rule leaves its room to a new one, even before the sweep.
    let again = add(&c, 1, &message("poll"), false, By::Agent, expires).unwrap();
    assert_eq!(
        again,
        Ok(Added {
            id: 2,
            created: true,
            taken_over: false
        })
    );
    counts::prune(&c, expires + 14 * DAY_MS).unwrap();
    assert_eq!(get(&c, 2).unwrap(), None);
}

#[test]
fn a_person_making_a_rule_live_holds_it_and_it_goes_last() {
    let c = memory();
    let agents = added(&c, &message("poll"), false, By::Agent).unwrap();
    added(&c, &message("health"), true, By::Person).unwrap();
    assert!(set_live(&c, agents.id, true, NOON_MS + 1).unwrap());
    let rule = get(&c, agents.id).unwrap().unwrap();
    assert_eq!(
        (
            rule.live,
            rule.by,
            rule.expires_at,
            rule.position,
            rule.changed_at
        ),
        (true, By::Person, None, 3, NOON_MS + 1)
    );
    assert_eq!(ids(&c, NOON_MS), [(2, true), (1, true)]);
    assert!(!set_live(&c, 9, true, NOON_MS).unwrap());
}

#[test]
fn an_agent_removes_only_its_own_shadow_rules() {
    let c = memory();
    let mine = added(&c, &message("a"), false, By::Agent).unwrap().id;
    let persons = added(&c, &message("b"), false, By::Person).unwrap().id;
    let live = added(&c, &message("c"), true, By::Person).unwrap().id;
    for id in [persons, live] {
        assert_eq!(
            remove_agent(&c, 1, id).unwrap(),
            Err(format!(
                "no shadow rule {id} of an agent in this project: a person removes the others with davy drop remove"
            ))
        );
    }
    assert!(
        remove_agent(&c, 2, mine).unwrap().is_err(),
        "another project"
    );
    let day = date(2026, 11, 25);
    write(
        &c,
        &[
            (key(day, mine, false, 1), n(3, 300)),
            (key(day, persons, false, 1), n(4, 400)),
        ],
    )
    .unwrap();
    assert_eq!(remove_agent(&c, 1, mine).unwrap(), Ok(()));
    assert_eq!(ids(&c, NOON_MS), [(persons, false), (live, true)]);
    // What the removed rule counted goes with it; the person's stays.
    let counted: Vec<(i64, i64)> = c
        .prepare("SELECT rule_id, records FROM drop_daily ORDER BY rule_id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(counted, [(persons, 4)]);
}

#[test]
fn an_agents_rule_that_expired_cannot_be_made_live_or_shadow() {
    let c = memory();
    let rule = added(&c, &message("poll"), false, By::Agent).unwrap().id;
    let expires = NOON_MS + 14 * DAY_MS;
    assert!(!set_live(&c, rule, true, expires).unwrap());
    assert!(!set_live(&c, rule, false, expires).unwrap());
    assert!(set_live(&c, rule, true, expires - 1).unwrap());
}

#[test]
fn a_pattern_rule_at_any_level_is_another_rule() {
    let c = memory();
    let below = Spec::new(Some("00000000abcd"), None, None, None, None).unwrap();
    let any = Spec {
        any_level: true,
        ..below.clone()
    };
    assert_eq!(any.options(), "--pattern 00000000abcd --any-level");
    assert_eq!(
        (below.hides_warnings(), any.hides_warnings()),
        (false, true)
    );
    assert_eq!(added(&c, &below, true, By::Person).map(|a| a.id), Ok(1));
    assert_eq!(added(&c, &any, true, By::Person).map(|a| a.id), Ok(2));
    assert!(get(&c, 2).unwrap().unwrap().spec.any_level);
    // Only a pattern rule has it.
    let refused = c.execute(
        "INSERT INTO drop_rules(project_id, position, live, by, message, keep_percent, any_level, created_at, changed_at) VALUES (1, 3, 1, 'person', 'x', 0, 1, 0, 0)",
        [],
    );
    assert!(refused.is_err());
}

fn key(day: Date, rule: i64, live: bool, project: i64) -> Key {
    Key {
        day,
        rule,
        live,
        project,
    }
}

fn n(records: u64, bytes: u64) -> Count {
    Count { records, bytes }
}

#[test]
fn live_rules_count_as_filtered_and_shadow_rules_do_not() {
    let c = memory();
    let today = date(2026, 11, 25);
    let yesterday = date(2026, 11, 24);
    write(
        &c,
        &[
            (key(yesterday, 1, true, 1), n(5, 500)),
            (key(today, 1, true, 1), n(2, 200)),
            (key(today, 2, false, 1), n(7, 700)),
            (key(today, 3, true, 2), n(1, 100)),
        ],
    )
    .unwrap();
    write(&c, &[(key(today, 1, true, 1), n(1, 100))]).unwrap();
    assert_eq!(
        filtered_between(&c, yesterday, today, None).unwrap(),
        HashMap::from([(1, n(8, 800)), (2, n(1, 100))])
    );
    assert_eq!(
        filtered_between(&c, today, today, Some(1)).unwrap(),
        HashMap::from([(1, n(3, 300))])
    );
    let counted = counts::by_rule(&c, yesterday, Some(1)).unwrap();
    assert_eq!(
        counted[&1],
        counts::Counted {
            dropped: n(8, 800),
            would_drop: n(0, 0),
            first: None,
        }
    );
    assert_eq!(
        counted[&2],
        counts::Counted {
            dropped: n(0, 0),
            would_drop: n(7, 700),
            first: Some(today),
        }
    );
}

#[test]
fn counts_of_a_project_that_is_gone_are_left_out() {
    let c = memory();
    write(&c, &[(key(date(2026, 11, 25), 1, true, 9), n(1, 1))]).unwrap();
    let rows: i64 = c
        .query_row("SELECT count(*) FROM drop_daily", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 0);
}

#[test]
fn counts_older_than_400_days_are_pruned() {
    let c = memory();
    let today = date(2026, 11, 25);
    let old = today.checked_sub(jiff::Span::new().days(400)).unwrap();
    let kept = old.tomorrow().unwrap();
    write(
        &c,
        &[
            (key(old, 1, true, 1), n(1, 1)),
            (key(kept, 1, true, 1), n(2, 2)),
        ],
    )
    .unwrap();
    counts::prune(&c, NOON_MS).unwrap();
    assert_eq!(
        filtered_between(&c, old, today, None).unwrap(),
        HashMap::from([(1, n(2, 2))])
    );
}

#[test]
fn removing_a_rule_keeps_its_counts_and_its_id_is_not_used_again() {
    let c = memory();
    let id = added(&c, &message("poll"), true, By::Person).unwrap().id;
    write(&c, &[(key(date(2026, 11, 25), id, true, 1), n(4, 40))]).unwrap();
    assert!(remove(&c, id).unwrap());
    assert!(!remove(&c, id).unwrap());
    let next = added(&c, &message("poll"), true, By::Person).unwrap().id;
    assert_eq!(next, id + 1);
    let day = date(2026, 11, 25);
    assert_eq!(
        filtered_between(&c, day, day, Some(1)).unwrap(),
        HashMap::from([(1, n(4, 40))])
    );
}

#[test]
fn an_equal_rule_in_the_other_state_is_refused_with_the_command_that_turns_it() {
    let c = memory();
    let agents = added(&c, &message("poll"), false, By::Agent).unwrap().id;
    assert_eq!(
        added(&c, &message("poll"), true, By::Person),
        Err(format!(
            "rule {agents} matches the same way and is a shadow rule: a person makes it live with davy drop live {agents}"
        ))
    );
    let persons = added(&c, &message("health"), true, By::Person).unwrap().id;
    assert_eq!(
        added(&c, &message("health"), false, By::Agent),
        Err(format!(
            "rule {persons} matches the same way and is live: a person makes it a shadow rule with davy drop shadow {persons}"
        ))
    );
    assert_eq!(ids(&c, NOON_MS), [(agents, false), (persons, true)]);
}

#[test]
fn a_person_who_adds_an_agents_shadow_rule_takes_it_over() {
    let c = memory();
    let staged = added(&c, &message("poll"), false, By::Agent).unwrap();
    assert_eq!(
        get(&c, staged.id).unwrap().unwrap().expires_at,
        Some(NOON_MS + 14 * DAY_MS)
    );
    assert_eq!(
        added(&c, &message("poll"), false, By::Person),
        Ok(Added {
            id: 1,
            created: false,
            taken_over: true
        })
    );
    let rule = get(&c, 1).unwrap().unwrap();
    assert_eq!(
        (rule.by, rule.expires_at, rule.live),
        (By::Person, None, false)
    );
    // Taken over once: adding it again finds a person's rule.
    assert_eq!(
        added(&c, &message("poll"), false, By::Person),
        Ok(Added {
            id: 1,
            created: false,
            taken_over: false
        })
    );
    // An agent adding a person's rule takes nothing over.
    assert_eq!(
        added(&c, &message("poll"), false, By::Agent),
        Ok(Added {
            id: 1,
            created: false,
            taken_over: false
        })
    );
    assert_eq!(get(&c, 1).unwrap().unwrap().by, By::Person);
}
