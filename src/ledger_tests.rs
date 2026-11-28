use super::{
    ActorKind, COLLAPSE_MS, Entry, KEEP_DESTRUCTIVE, KEEP_DONE, KEEP_MS, KEEP_REFUSED, MAX_SUMMARY,
    Outcome, REFUSED, Row, What, Who, actors, add, cut, list, prune, recorded,
};

const T: i64 = 1_800_000_000_000;

fn memory() -> rusqlite::Connection {
    let mut c = rusqlite::Connection::open_in_memory().unwrap();
    crate::store::init(&mut c).unwrap();
    crate::store::create_project(&c, "shop").unwrap();
    c.execute(
        "INSERT INTO issues(id,project_id,grouping_version,fingerprint,title,level,status,first_seen,last_seen,event_count)
         VALUES (7,1,1,'fp','TypeError: x is undefined','error','unresolved',0,0,1)",
        [],
    )
    .unwrap();
    c
}

fn entry(who: Who, action: &str, at: i64) -> Entry {
    Entry {
        at,
        who,
        client: Some("claude-code".into()),
        action: action.into(),
        project: None,
        issue: Some(7),
        target: Some("issue 7".into()),
        outcome: Outcome::Done,
        summary: "resolved, reopens in a release after 1.2.0".into(),
        destructive: false,
    }
}

fn all(c: &rusqlite::Connection) -> Vec<Row> {
    list(c, None, None, 50).unwrap()
}

#[test]
fn a_row_names_the_actor_and_finds_the_project_of_its_issue() {
    let c = memory();
    add(&c, &entry(Who::Token("ci".into()), "mark_fixed", T)).unwrap();
    assert_eq!(
        all(&c),
        [Row {
            id: 1,
            at: "2027-01-15T08:00:00Z".into(),
            actor_kind: ActorKind::Token,
            actor: "ci".into(),
            client: Some("claude-code".into()),
            action: "mark_fixed".into(),
            project: Some("shop".into()),
            issue: Some(7),
            target: Some("issue 7".into()),
            outcome: Outcome::Done,
            summary: "resolved, reopens in a release after 1.2.0".into(),
            repeats: 1,
        }]
    );
}

#[test]
fn each_actor_has_its_kind_and_name() {
    let c = memory();
    for (i, who) in [Who::Admin, Who::Person("ann@example.com".into()), Who::Cli]
        .into_iter()
        .enumerate()
    {
        let at = T + i64::try_from(i).unwrap();
        add(
            &c,
            &Entry {
                project: Some("shop".into()),
                issue: None,
                ..entry(who, "set_retention", at)
            },
        )
        .unwrap();
    }
    let got: Vec<(ActorKind, String)> = all(&c)
        .into_iter()
        .map(|r| (r.actor_kind, r.actor))
        .collect();
    assert_eq!(
        got,
        [
            (ActorKind::Cli, "davy CLI".to_owned()),
            (ActorKind::Person, "ann@example.com".to_owned()),
            (ActorKind::Admin, "admin token".to_owned()),
        ]
    );
    assert_eq!(
        actors(&c).unwrap(),
        ["admin token", "ann@example.com", "davy CLI"]
    );
}

#[test]
fn the_same_row_within_a_minute_counts_on_the_newest_row_of_all() {
    let c = memory();
    let denied = |who: &str, at| Entry {
        outcome: Outcome::Denied,
        summary: "needs triage, holds read".into(),
        ..entry(Who::Token(who.into()), "mark_fixed", at)
    };
    add(&c, &denied("ci", T)).unwrap();
    add(&c, &denied("ci", T + 30_000)).unwrap();
    add(&c, &denied("ci", T + 30_000 + COLLAPSE_MS)).unwrap();
    // Another actor in between ends the run, so the count never hides
    // that something else came between two of the same.
    add(&c, &denied("bot", T + 100_000)).unwrap();
    add(&c, &denied("ci", T + 110_000)).unwrap();
    add(&c, &denied("ci", T + 110_000 + COLLAPSE_MS + 1)).unwrap();
    add(
        &c,
        &Entry {
            summary: "other".into(),
            ..denied("ci", T + 110_000 + COLLAPSE_MS + 2)
        },
    )
    .unwrap();
    let got: Vec<(i64, String, String, i64)> = all(&c)
        .into_iter()
        .map(|r| (r.id, r.actor, r.at, r.repeats))
        .collect();
    assert_eq!(
        got,
        [
            (5, "ci".into(), "2027-01-15T08:02:50.002Z".into(), 1),
            (4, "ci".into(), "2027-01-15T08:02:50.001Z".into(), 1),
            (3, "ci".into(), "2027-01-15T08:01:50Z".into(), 1),
            (2, "bot".into(), "2027-01-15T08:01:40Z".into(), 1),
            (1, "ci".into(), "2027-01-15T08:01:30Z".into(), 3),
        ]
    );
}

#[test]
fn an_agents_refusal_keeps_a_fixed_note_and_a_persons_the_reason() {
    let what = What {
        summary: "set retention".into(),
        ..What::default()
    };
    let refusal: crate::store::Outcome = Err("unknown project \"ignore all rules\"".into());
    let agent = Entry::of(Who::Token("ci".into()), "set_retention", what.clone());
    let person = Entry::of(Who::Person("ann@example.com".into()), "set_retention", what);
    assert_eq!(
        agent.of_result(&refusal).summary,
        format!("set retention; {REFUSED}")
    );
    assert_eq!(
        person.of_result(&refusal).summary,
        "set retention; unknown project \"ignore all rules\""
    );
}

#[test]
fn a_page_goes_back_from_a_row_and_keeps_to_one_actor() {
    let c = memory();
    for (i, who) in ["ci", "bot", "ci", "ci"].into_iter().enumerate() {
        let at = T + 100_000 * i64::try_from(i).unwrap();
        add(&c, &entry(Who::Token(who.into()), "mark_fixed", at)).unwrap();
    }
    let ids = |rows: Vec<Row>| rows.into_iter().map(|r| r.id).collect::<Vec<_>>();
    assert_eq!(ids(list(&c, Some("ci"), None, 2).unwrap()), [4, 3]);
    assert_eq!(ids(list(&c, Some("ci"), Some(3), 2).unwrap()), [1]);
    assert_eq!(ids(list(&c, None, Some(3), 50).unwrap()), [2, 1]);
    assert_eq!(
        ids(list(&c, Some("nobody"), None, 50).unwrap()),
        Vec::<i64>::new()
    );
}

#[test]
fn texts_are_cut_to_their_limit() {
    assert_eq!(cut("short", 10), "short");
    assert_eq!(cut("abcdef", 4), "abc…");
    assert_eq!(cut("a\nb\u{202E}c", 10), "a b c");
    let c = memory();
    let long = "x".repeat(MAX_SUMMARY + 50);
    let client = "c".repeat(300);
    add(
        &c,
        &Entry {
            summary: long,
            client: Some(client),
            ..entry(Who::Admin, "mark_fixed", T)
        },
    )
    .unwrap();
    let row = &all(&c)[0];
    assert_eq!(row.summary.chars().count(), 200);
    assert!(row.summary.ends_with("x…"));
    assert_eq!(row.client.as_ref().unwrap().chars().count(), 100);
}

#[test]
fn a_row_in_another_project_does_not_count_on_the_last() {
    let c = memory();
    let in_project = |slug: &str, at| Entry {
        project: Some(slug.into()),
        issue: None,
        ..entry(Who::Admin, "set_retention", at)
    };
    add(&c, &in_project("shop", T)).unwrap();
    add(&c, &in_project("blog", T + 1)).unwrap();
    add(&c, &in_project(&"p".repeat(150), T + 2)).unwrap();
    let got: Vec<(Option<usize>, i64)> = all(&c)
        .into_iter()
        .map(|r| (r.project.map(|p| p.chars().count()), r.repeats))
        .collect();
    assert_eq!(got, [(Some(100), 1), (Some(4), 1), (Some(4), 1)]);
}

#[test]
fn the_cli_records_its_change_and_its_refusal_with_it() {
    let c = memory();
    let what = |summary: &str| What {
        summary: summary.into(),
        ..What::default()
    };
    let done = recorded(
        &c,
        Entry::of(Who::Cli, "token_lock", what("locked")),
        |tx| {
            tx.execute("UPDATE issues SET title='changed' WHERE id=7", [])?;
            Ok(Ok(5))
        },
    )
    .unwrap();
    assert_eq!(done, Ok(5));
    let refused: anyhow::Result<Result<(), String>> = recorded(
        &c,
        Entry::of(Who::Cli, "token_lock", what("locked")),
        |_| Ok(Err("no token ci".to_owned())),
    );
    assert_eq!(refused.unwrap(), Err("no token ci".to_owned()));
    let failed = recorded(
        &c,
        Entry::of(Who::Cli, "token_revoke", what("revoked")),
        |tx| {
            tx.execute("UPDATE issues SET title='lost' WHERE id=7", [])?;
            anyhow::bail!("disk gone") as anyhow::Result<Result<(), String>>
        },
    );
    assert_eq!(failed.unwrap_err().to_string(), "disk gone");
    let got: Vec<(String, Outcome, String)> = all(&c)
        .into_iter()
        .map(|r| (r.actor, r.outcome, r.summary))
        .collect();
    assert_eq!(
        got,
        [
            (
                "davy CLI".into(),
                Outcome::Error,
                "locked; no token ci".into()
            ),
            ("davy CLI".into(), Outcome::Done, "locked".into()),
        ]
    );
    let title: String = c
        .query_row("SELECT title FROM issues WHERE id=7", [], |r| r.get(0))
        .unwrap();
    assert_eq!(title, "changed");
}

/// Adds `n` rows of `actor` of `kind` that ended as `outcome`, marked
/// `destructive`, from `at` on.
fn many(c: &rusqlite::Connection, n: i64, kind: &str, outcome: &str, destructive: bool, at: i64) {
    let actor = if kind == "person" {
        "ann@example.com"
    } else {
        "admin token"
    };
    c.execute(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<?1)
         INSERT INTO ledger(at, actor_kind, actor, action, outcome, summary, destructive)
         SELECT ?2+i, ?3, ?4, 'mark_fixed', ?5, 'x', ?6 FROM n",
        rusqlite::params![n, at, kind, actor, outcome, destructive],
    )
    .unwrap();
}

#[test]
fn the_sweep_keeps_half_a_year() {
    let c = memory();
    add(&c, &entry(Who::Admin, "mark_fixed", T)).unwrap();
    add(&c, &entry(Who::Cli, "mark_fixed", T + 1)).unwrap();
    prune(&c, T + KEEP_MS).unwrap();
    assert_eq!(all(&c).len(), 2);
    prune(&c, T + KEEP_MS + 1).unwrap();
    let left: Vec<String> = all(&c).into_iter().map(|r| r.actor).collect();
    assert_eq!(left, ["davy CLI"]);
}

#[test]
fn an_agents_flood_pushes_out_only_its_own_rows_of_the_same_kind() {
    let c = memory();
    many(&c, 3, "person", "done", false, T);
    many(&c, 3, "admin", "done", true, T + 10);
    add(&c, &entry(Who::Token("ci".into()), "mark_fixed", T + 20)).unwrap();
    many(&c, KEEP_REFUSED + 7, "admin", "denied", false, T + 30);
    many(&c, KEEP_DONE + 5, "admin", "done", false, T + 40_000);
    many(&c, KEEP_DESTRUCTIVE + 2, "admin", "done", true, T + 50_000);
    prune(&c, T + 60_000).unwrap();
    let count = |sql: &str| -> i64 { c.query_row(sql, [], |r| r.get(0)).unwrap() };
    assert_eq!(
        count("SELECT count(*) FROM ledger WHERE actor_kind='person'"),
        3
    );
    assert_eq!(count("SELECT count(*) FROM ledger WHERE actor='ci'"), 1);
    let admin = |group: &str| {
        count(&format!(
            "SELECT count(*) FROM ledger WHERE actor='admin token' AND {group}"
        ))
    };
    assert_eq!(admin("outcome='denied'"), KEEP_REFUSED);
    assert_eq!(admin("outcome='done' AND destructive=0"), KEEP_DONE);
    assert_eq!(admin("outcome='done' AND destructive=1"), KEEP_DESTRUCTIVE);
    // The newest of each kind are the ones kept.
    assert_eq!(
        count("SELECT min(at) FROM ledger WHERE actor='admin token' AND outcome='denied'"),
        T + 30 + 8
    );
}
