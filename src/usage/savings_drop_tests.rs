use super::*;
use crate::{
    drops::{self, By as Who, Key, Match, Spec},
    usage::Count,
};

fn shop(top: usize) -> Request {
    Request {
        project: Some("shop".into()),
        top,
        defaults: retention::Defaults::default(),
    }
}

fn kinds(view: &Value) -> Vec<&str> {
    view["savings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect()
}

/// Adds to shop, 2 days before `now`, an agent's shadow rule that would
/// have dropped 90 lines of 2,000 bytes since and a person's live rule,
/// and gives the shadow rule's id and when both were made.
fn two_rules(c: &Connection, now: i64) -> (i64, i64) {
    let made = now / 1_000_000 - 2 * DAY_MS;
    let health = health();
    let spec = Spec::new(Some(&health), None, None, None, Some("10%")).unwrap();
    let id = drops::add(c, 1, &spec, false, Who::Agent, made)
        .unwrap()
        .unwrap()
        .id;
    let debug = Spec::new(None, Some("debug"), None, None, None).unwrap();
    drops::add(c, 1, &debug, true, Who::Person, made)
        .unwrap()
        .unwrap();
    count(c, (id, false), time::day_of(made * 1_000_000), (90, 2_000));
    (id, made)
}

/// The pattern of the health lines.
fn health() -> String {
    patterns::show(patterns::of("GET /health 200 12ms").id)
}

/// Writes what rule `id`, live or shadow, counted on `day`.
fn count(c: &Connection, (id, live): (i64, bool), day: Date, (records, bytes): (u64, u64)) {
    let key = Key {
        day,
        rule: id,
        live,
        project: 1,
    };
    drops::write(c, &[(key, Count { records, bytes })]).unwrap();
}

#[test]
fn a_shadow_rule_shows_what_it_would_drop_and_the_command_to_make_it_live() {
    let (data, now) = data_with_savings("savings-shadow");
    let c = store::open(&data).unwrap();
    let (id, made) = two_rules(&c, now);
    let found = savings(&c, &data, &week(now), &shop(2), now).unwrap();
    let view = found.json(false);
    let d = sh(&data.display().to_string());
    // The live rule is no entry: what it drops is logs_filtered.
    assert_eq!(
        kinds(&view),
        [
            "pattern",
            "pattern",
            "shadow",
            "retention",
            "issue",
            "issue"
        ]
    );
    assert_eq!(
        view["savings"][2],
        json!({
            "kind": "shadow", "id": id, "project": "shop", "by": "agent",
            "pattern": health(), "level": null, "message": null, "service": null,
            "keep_percent": 10,
            "created_at": time::rfc3339_ms(made),
            "expires_at": time::rfc3339_ms(made + 14 * DAY_MS),
            "hours": 48.0,
            "would_drop": {"records": 90, "bytes": 2_000},
            "saves_per_day": 1_000,
            "hides": null,
            "suggest": format!("make it live; a person can run: davy drop live --data {d} {id}"),
        })
    );
    let human = found.json(true);
    assert_eq!(
        [
            &human["savings"][2]["would_drop"]["bytes"],
            &human["savings"][2]["saves_per_day"]
        ],
        ["2.0 KB", "1.0 KB"]
    );
    let mut out = Vec::new();
    found.text(&mut out).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(
        text.contains(&format!(
            "shadow    rule {id} in shop by agent: pattern {} below warn, keep 10%: would drop 90 lines (2.0 KB) in 2.0 days, 1.0 KB/day\n          \
             a person can run: davy drop live --data {d} {id}\n",
            health()
        )),
        "{text}"
    );
    // The view of every project shows it too.
    let all = Request {
        project: None,
        ..shop(2)
    };
    let every = savings(&c, &data, &week(now), &all, now).unwrap();
    assert_eq!(
        every
            .shadow
            .iter()
            .map(|s| (s.rule.id, s.project.as_str()))
            .collect::<Vec<_>>(),
        [(id, "shop")]
    );
    std::fs::remove_dir_all(&data).unwrap();
}

#[test]
fn a_pattern_gets_a_share_to_keep_in_one_projects_view_unless_a_rule_names_it() {
    let (data, now) = data_with_savings("savings-keep");
    let c = store::open(&data).unwrap();
    let now_ms = now / 1_000_000;
    // A search found the health lines, so the view keeps 10% of them.
    add(&c, now_ms, &search(Some(1), DAY_NS, Some("health"))(now_ms)).unwrap();
    let report = week(now);
    let keeps = |ask: &Request| {
        savings(&c, &data, &report, ask, now)
            .unwrap()
            .patterns
            .iter()
            .map(|p| p.keep)
            .collect::<Vec<_>>()
    };
    let ten = Some(Keep {
        percent: KEEP_READ,
        saves_per_day: 296,
    });
    // The second pattern is of errors.
    assert_eq!(keeps(&shop(2)), [ten, None]);
    let found = savings(&c, &data, &report, &shop(1), now).unwrap();
    assert_eq!(found.json(true)["savings"][0]["saves_per_day"], "296 B");
    let mut out = Vec::new();
    found.text(&mut out).unwrap();
    let (text, d) = (
        String::from_utf8(out).unwrap(),
        sh(&data.display().to_string()),
    );
    let id = patterns::show(patterns::of("GET /health 200 12ms").id);
    assert!(
        text.contains(&format!(
            "          GET /health 200 12ms\n          \
             keep 10% to save 296 B/day\n          \
             a person can run: davy drop add --pattern {id} --keep 10% --data {d} -- shop\n          \
             try: search_logs pattern={id}\n"
        )),
        "{text}"
    );
    let all = Request {
        project: None,
        ..shop(2)
    };
    assert_eq!(keeps(&all), [None, None], "a rule is of one project");
    c.execute("UPDATE projects SET own=1 WHERE slug='shop'", [])
        .unwrap();
    assert_eq!(keeps(&shop(2)), [None, None], "Davy's own project");
    c.execute("UPDATE projects SET own=0 WHERE slug='shop'", [])
        .unwrap();
    let health = Spec {
        matches: Match::Pattern(patterns::of("GET /health 200 12ms").id),
        service: None,
        keep: 0,
        any_level: false,
    };
    drops::add(&c, 1, &health, false, Who::Person, now_ms)
        .unwrap()
        .unwrap();
    assert_eq!(keeps(&shop(2)), [None, None], "a rule names it");
    std::fs::remove_dir_all(&data).unwrap();
}

#[test]
fn a_pattern_whose_lines_reach_warn_gets_no_share_to_keep() {
    let p = Pattern {
        id: patterns::show(patterns::of("job 1 done").id),
        project: Some("shop".into()),
        service: None,
        level: Some("info"),
        example: "job 1 done".into(),
        bytes: 7_000,
        per_day: 1_000,
        share: 1.0,
        last_queried: None,
        keep: None,
        decided_by: None,
    };
    let keep = |level: Option<LogLevel>| drop::keep(&p, level.map(|l| l as u8), &[]);
    assert_eq!(
        keep(Some(LogLevel::Info)),
        Some(Keep {
            percent: KEEP_UNREAD,
            saves_per_day: 990
        })
    );
    assert_eq!(keep(Some(LogLevel::Warn)), None);
    assert_eq!(keep(None), None, "no level known");
}

#[test]
fn a_pattern_a_live_rule_decides_gets_no_share_to_keep() {
    let (data, now) = data_with_savings("savings-decided");
    let c = store::open(&data).unwrap();
    let now_ms = now / 1_000_000;
    let health = Spec::new(None, None, Some("/health"), None, None).unwrap();
    let id = drops::add(&c, 1, &health, true, Who::Person, now_ms)
        .unwrap()
        .unwrap()
        .id;
    let found = savings(&c, &data, &week(now), &shop(1), now).unwrap();
    assert_eq!(
        found
            .patterns
            .iter()
            .map(|p| (p.keep, p.decided_by))
            .collect::<Vec<_>>(),
        [(None, Some(id))]
    );
    let entry = &found.json(false)["savings"][0];
    assert_eq!(
        [&entry["suggest"], &entry["saves_per_day"], &entry["shadow"]],
        [
            &json!(format!("an earlier rule decides these lines: rule {id}")),
            &Value::Null,
            &Value::Null
        ]
    );
    let mut out = Vec::new();
    found.text(&mut out).unwrap();
    let text = String::from_utf8(out).unwrap();
    let pattern = patterns::show(patterns::of("GET /health 200 12ms").id);
    assert!(
        text.contains(&format!(
            "          GET /health 200 12ms\n          \
             an earlier rule decides these lines: rule {id}\n          \
             try: search_logs pattern={pattern}\n"
        )),
        "{text}"
    );
    std::fs::remove_dir_all(&data).unwrap();
}

#[test]
fn a_shadow_rule_divides_by_the_days_since_its_first_counted_day() {
    let data = scratch("savings-shadow-days");
    let c = store::open(&data).unwrap();
    store::create_project(&c, "shop").unwrap();
    // 2026-11-20 at noon UTC.
    let day = |d: i8| Date::new(2026, 11, d).unwrap();
    let now_ms = time::midnight(day(20)) / 1_000_000 + 12 * HOUR_MS;
    let made = time::midnight(day(10)) / 1_000_000 + 12 * HOUR_MS;
    let add = |level: &str, at: i64| {
        let spec = Spec::new(None, Some(level), None, None, None).unwrap();
        drops::add(&c, 1, &spec, false, Who::Person, at)
            .unwrap()
            .unwrap()
            .id
    };
    // Made 5 hours ago: no number a day yet.
    let young = add("trace", now_ms - 5 * HOUR_MS);
    count(&c, (young, false), day(20), (90, 2_000));
    // Live on the 15th and 16th, a shadow rule again since the 18th.
    let back = add("debug", made);
    count(&c, (back, true), day(15), (500, 50_000));
    count(&c, (back, true), day(16), (500, 50_000));
    count(&c, (back, false), day(18), (40, 4_000));
    count(&c, (back, false), day(19), (10, 1_000));
    // Counted nothing in the window, which starts on the 14th.
    let quiet = add("info", made);
    let rules = drops::load(&c, Some(1), now_ms).unwrap();
    let slug = |_: i64| "shop".to_owned();
    let found = drop::shadows(&c, rules, (&slug, None), day(14), now_ms, 3).unwrap();
    assert_eq!(
        found
            .iter()
            .map(|s| (s.rule.id, s.hours, s.per_day))
            .collect::<Vec<_>>(),
        [
            (back, 60.0, Some(2_000)),
            (young, 5.0, None),
            (quiet, 156.0, Some(0))
        ]
    );
    let view = Savings {
        since: day(14),
        days: 7.0,
        patterns: Vec::new(),
        partial: false,
        shadow: found,
        retention: Vec::new(),
        issues: Vec::new(),
        data: "/d".into(),
    };
    assert_eq!(
        view.shadow_text(&view.shadow[1]),
        [
            format!(
                "rule {young} in shop by person: level trace and below, drop all: \
                 would drop 90 lines (2.0 KB) in its first 5.0 hours, per day after a full day"
            ),
            format!("a person can run: davy drop live --data /d {young}")
        ]
    );
    let entry = view.json(false)["savings"][1].clone();
    assert_eq!(
        [&entry["hours"], &entry["saves_per_day"]],
        [&json!(5.0), &Value::Null]
    );
    std::fs::remove_dir_all(&data).unwrap();
}

#[test]
fn a_shadow_rule_that_would_hide_warnings_or_davys_lines_gets_no_command() {
    let (data, now) = data_with_savings("savings-shadow-own");
    let c = store::open(&data).unwrap();
    c.execute("UPDATE projects SET own=1 WHERE slug='shop'", [])
        .unwrap();
    let now_ms = now / 1_000_000 - HOUR_MS;
    let debug = Spec::new(None, Some("debug"), None, None, None).unwrap();
    let own = drops::add(&c, 1, &debug, false, Who::Person, now_ms)
        .unwrap()
        .unwrap()
        .id;
    let health = Spec::new(None, None, Some("health"), None, None).unwrap();
    let message = drops::add(&c, 2, &health, false, Who::Person, now_ms)
        .unwrap()
        .unwrap()
        .id;
    let all = Request {
        project: None,
        ..shop(2)
    };
    let found = savings(&c, &data, &week(now), &all, now).unwrap();
    let view = found.json(false);
    let shadows: Vec<_> = view["savings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "shadow")
        .map(|e| (e["id"].clone(), e["hides"].clone(), e["suggest"].clone()))
        .collect();
    assert_eq!(
        shadows,
        [
            (
                json!(own),
                json!("lines of Davy's own project, which davy doctor and project_status read"),
                Value::Null
            ),
            (
                json!(message),
                json!("every line that holds the text, warnings and errors included"),
                Value::Null
            ),
        ]
    );
    let mut out = Vec::new();
    found.text(&mut out).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(
        text.contains(
            "          it would hide every line that holds the text, warnings and errors included\n"
        ),
        "{text}"
    );
    assert!(!text.contains("davy drop live"), "{text}");
    std::fs::remove_dir_all(&data).unwrap();
}
