use super::{
    ActorKind, COLLAPSE_MS, DONE, Entry, KEEP_DONE, KEEP_MS, KEEP_REFUSED, MAX_SUMMARY, Outcome,
    PRUNE_BATCH, REFUSED, REFUSED_ROWS, Row, What, Who, actors, add, cut, list, prune, recorded,
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

/// Adds `n` rows of the actor of `kind` that ended as `outcome`, marked
/// `destructive`, from `at` on: `ann@example.com` for a person, `davy CLI`
/// for the CLI and the admin token otherwise. The admin token's rows of a
/// capped group take its next numbers, as [`add`] would give them.
fn many(c: &rusqlite::Connection, n: i64, kind: &str, outcome: &str, destructive: bool, at: i64) {
    let (actor, group) = match (kind, outcome, destructive) {
        ("person", ..) => ("ann@example.com", None),
        ("cli", ..) => ("davy CLI", None),
        (_, "done", true) => ("admin token", None),
        (_, "done", false) => ("admin token", Some(DONE.0)),
        _ => ("admin token", Some(REFUSED_ROWS.0)),
    };
    let next: Option<i64> = group.map(|terms| {
        c.query_row(&super::numbered(terms), [actor], |r| r.get(0))
            .unwrap()
    });
    c.execute(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<?1)
         INSERT INTO ledger(at, actor_kind, actor, action, outcome, summary, destructive, seq)
         SELECT ?2+i, ?3, ?4, 'mark_fixed', ?5, 'x', ?6, ?7+i-1 FROM n",
        rusqlite::params![n, at, kind, actor, outcome, destructive, next],
    )
    .unwrap();
}

fn count(c: &rusqlite::Connection, sql: &str) -> i64 {
    c.query_row(sql, [], |r| r.get(0)).unwrap()
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
fn one_sweep_deletes_one_batch_of_the_oldest() {
    let c = memory();
    many(&c, PRUNE_BATCH + 3, "person", "done", false, T);
    prune(&c, T + KEEP_MS + PRUNE_BATCH + 10).unwrap();
    assert_eq!(count(&c, "SELECT count(*) FROM ledger"), 3);
    assert_eq!(count(&c, "SELECT min(at) FROM ledger"), T + PRUNE_BATCH + 1);
    prune(&c, T + KEEP_MS + PRUNE_BATCH + 10).unwrap();
    assert_eq!(count(&c, "SELECT count(*) FROM ledger"), 0);
}

#[test]
fn an_agents_flood_pushes_out_only_its_own_rows_of_the_same_kind() {
    let c = memory();
    many(&c, 3, "person", "done", false, T);
    many(&c, 3, "admin", "done", true, T + 10);
    add(&c, &entry(Who::Token("ci".into()), "mark_fixed", T + 20)).unwrap();
    many(&c, KEEP_REFUSED + 7, "admin", "denied", false, T + 30);
    many(&c, KEEP_DONE + 5, "admin", "done", false, T + 40_000);
    many(&c, 2_500, "admin", "done", true, T + 50_000);
    // Rows are capped as they come: one more of each kind.
    for (outcome, destructive) in [
        (Outcome::Denied, false),
        (Outcome::Done, false),
        (Outcome::Done, true),
    ] {
        let row = Entry {
            outcome,
            destructive,
            ..entry(Who::Admin, "mark_fixed", T + 60_000)
        };
        add(&c, &row).unwrap();
    }
    assert_eq!(
        count(&c, "SELECT count(*) FROM ledger WHERE actor_kind='person'"),
        3
    );
    assert_eq!(count(&c, "SELECT count(*) FROM ledger WHERE actor='ci'"), 1);
    let admin = |group: &str| {
        count(
            &c,
            &format!("SELECT count(*) FROM ledger WHERE actor='admin token' AND {group}"),
        )
    };
    assert_eq!(admin("outcome='denied'"), KEEP_REFUSED);
    assert_eq!(admin("outcome='done' AND destructive=0"), KEEP_DONE);
    // No count pushes out a change that deleted data, so a token cannot
    // erase the record of its own.
    assert_eq!(admin("outcome='done' AND destructive=1"), 3 + 2_500 + 1);
    // The newest of each kind are the ones kept.
    assert_eq!(
        count(
            &c,
            "SELECT min(at) FROM ledger WHERE actor='admin token' AND outcome='denied'"
        ),
        T + 30 + 9
    );
}

#[test]
fn a_persons_rows_and_the_clis_have_no_cap() {
    let c = memory();
    many(&c, KEEP_DONE + 5, "person", "done", false, T);
    many(&c, KEEP_REFUSED + 5, "cli", "error", false, T + 10_000);
    add(
        &c,
        &entry(
            Who::Person("ann@example.com".into()),
            "mark_fixed",
            T + 20_000,
        ),
    )
    .unwrap();
    let refused = Entry {
        outcome: Outcome::Error,
        ..entry(Who::Cli, "mark_fixed", T + 20_001)
    };
    add(&c, &refused).unwrap();
    assert_eq!(
        count(&c, "SELECT count(*) FROM ledger WHERE actor_kind='person'"),
        KEEP_DONE + 6
    );
    assert_eq!(
        count(&c, "SELECT count(*) FROM ledger WHERE actor_kind='cli'"),
        KEEP_REFUSED + 6
    );
    // Nor are they numbered, as no cap counts them.
    assert_eq!(count(&c, "SELECT count(seq) FROM ledger"), 0);
}

#[test]
fn a_change_that_deleted_data_never_counts_on_one_that_did_not() {
    let c = memory();
    let row = entry(Who::Token("ci".into()), "set_retention", T);
    add(&c, &row).unwrap();
    add(&c, &Entry { at: T + 1, ..row }.destructive()).unwrap();
    let got: Vec<i64> = all(&c).into_iter().map(|r| r.repeats).collect();
    assert_eq!(got, [1, 1]);
}

/// The steps SQLite plans for `sql`.
fn plan(c: &rusqlite::Connection, sql: &str) -> Vec<String> {
    let mut q = c.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
    let n = q.parameter_count();
    q.query_map(rusqlite::params_from_iter(vec![1_i64; n]), |r| r.get(3))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

#[test]
fn the_caps_and_the_sweep_read_their_own_indexes() {
    let c = memory();
    for ((group, _), index) in [(DONE, "ledger_done"), (REFUSED_ROWS, "ledger_refused")] {
        // One search each, for the newest number and for the range past
        // the cap: no step walks the rows the cap keeps.
        for (sql, search) in [
            (super::numbered(group), format!("INDEX {index} (actor=?)")),
            (
                super::capped(group),
                format!("INDEX {index} (actor=? AND seq<?)"),
            ),
        ] {
            let steps = plan(&c, &sql);
            assert!(
                steps.len() == 1
                    && steps[0].starts_with("SEARCH ledger")
                    && steps[0].contains(&search),
                "{sql}: {steps:?}"
            );
        }
    }
    let steps = plan(&c, super::PRUNE);
    assert!(
        steps.iter().any(|s| s.contains("INDEX ledger_at")),
        "{steps:?}"
    );
}

/// Migration 28 numbers the rows a ledger of migration 27 holds, so the
/// first row added after it pushes out the oldest past the cap.
#[test]
fn the_migration_numbers_the_rows_there_and_caps_go_on_from_them() {
    let c = memory();
    c.execute_batch(&format!("{} {}", super::UNDO, super::MIGRATION))
        .unwrap();
    for (kind, actor, outcome, destructive) in [
        ("admin", "admin token", "done", 0),
        ("token", "ci", "denied", 0),
        ("token", "ci", "done", 0),
        ("token", "ci", "error", 0),
        ("token", "ci", "done", 1),
        ("person", "ann@example.com", "done", 0),
        ("token", "ci", "done", 0),
    ] {
        c.execute(
            "INSERT INTO ledger(at, actor_kind, actor, action, outcome, summary, destructive)
             VALUES (?1, ?2, ?3, 'mark_fixed', ?4, 'x', ?5)",
            rusqlite::params![T, kind, actor, outcome, destructive],
        )
        .unwrap();
    }
    c.execute_batch(super::CAPS).unwrap();
    let mut q = c.prepare("SELECT seq FROM ledger ORDER BY id").unwrap();
    let got: Vec<Option<i64>> = q
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        got,
        [Some(1), Some(1), Some(1), Some(2), None, None, Some(2)]
    );
    c.execute(
        "WITH RECURSIVE n(i) AS (SELECT 3 UNION ALL SELECT i+1 FROM n WHERE i<?1)
         INSERT INTO ledger(at, actor_kind, actor, action, outcome, summary, seq)
         SELECT ?2+i, 'token', 'ci', 'mark_fixed', 'done', 'x', i FROM n",
        rusqlite::params![KEEP_DONE, T],
    )
    .unwrap();
    let ci = |c: &rusqlite::Connection, what: &str| {
        count(
            c,
            &format!(
                "SELECT {what} FROM ledger WHERE actor='ci' AND outcome='done' AND destructive=0"
            ),
        )
    };
    assert_eq!(ci(&c, "count(*)"), KEEP_DONE);
    add(
        &c,
        &entry(Who::Token("ci".into()), "mark_fixed", T + 10_000),
    )
    .unwrap();
    // The row numbered 1 by the migration is the one pushed out.
    assert_eq!(ci(&c, "count(*)"), KEEP_DONE);
    assert_eq!(ci(&c, "min(seq)"), 2);
    assert_eq!(ci(&c, "max(seq)"), KEEP_DONE + 1);
}
