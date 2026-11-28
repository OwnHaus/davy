//! What the writer reads again before a change an unlocked token asked
//! for as destructive, and how the ledger marks its row.

use super::{
    Change, MADE, MAX_DESTRUCTIVE, NO_LONGER_UNLOCKED, TOO_MANY, create, find, hash, refusal,
};
use crate::{
    access::{Actor, Caller, Level, Unlocked},
    ledger::{Entry, What, Who},
    retention::{By, Defaults, Setting},
    store::{self, Job, Write},
    tokens::Name,
};

const T: i64 = 1_800_000_000_000;

fn memory() -> rusqlite::Connection {
    let mut c = rusqlite::Connection::open_in_memory().unwrap();
    store::init(&mut c).unwrap();
    store::create_project(&c, "api").unwrap();
    c
}

fn unlock(name: &str, at: i64) -> Change {
    Change::Unlock {
        name: name.into(),
        by: "ann@example.com".into(),
        at,
    }
}

/// Adds token `name` of level full, unlocked at `T + 5`, and returns the
/// proof a call of it carries.
fn unlocked(c: &rusqlite::Connection, name: &str) -> Unlocked {
    let parsed = Name::parse(name).unwrap();
    let secret = create(c, &parsed, Level::Full, T).unwrap().unwrap().1;
    super::apply(c, &unlock(name, T + 5)).unwrap().unwrap();
    let found = find(c, &hash(&secret), T).unwrap().unwrap();
    let caller = Caller {
        actor: Actor::Token {
            id: found.id,
            name: found.name,
        },
        grant: found.grant,
    };
    caller.unlocked().unwrap()
}

/// Adds `n` rows of token `name` that ended as `outcome`, marked
/// `destructive`, at `at`.
fn rows(c: &rusqlite::Connection, name: &str, n: i64, outcome: &str, destructive: bool, at: i64) {
    c.execute(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<?1)
         INSERT INTO ledger(at, actor_kind, actor, action, outcome, summary, destructive)
         SELECT ?2, 'token', ?3, 'set_retention', ?4, 'logs 1 days', ?5 FROM n",
        rusqlite::params![n, at, name, outcome, destructive],
    )
    .unwrap();
}

#[test]
fn a_token_makes_at_most_so_many_destructive_changes_per_unlock() {
    let c = memory();
    let proof = unlocked(&c, "ci");
    // Before the unlock, refused, and of another token: none counts.
    rows(&c, "ci", 5, "done", true, T);
    rows(&c, "ci", 5, "error", true, T + 10);
    rows(&c, "ops", 5, "done", true, T + 10);
    rows(&c, "ci", MAX_DESTRUCTIVE - 1, "done", true, T + 10);
    assert_eq!(refusal(&c, proof).unwrap(), None);
    rows(&c, "ci", 1, "done", true, T + 20);
    assert_eq!(refusal(&c, proof).unwrap().as_deref(), Some(TOO_MANY));
    assert!(TOO_MANY.contains(&format!(" {MAX_DESTRUCTIVE} ")));
    // Unlocking an unlocked token keeps its unlock; a lock and a new
    // unlock start the count again.
    super::apply(&c, &unlock("ci", T + 30)).unwrap().unwrap();
    assert_eq!(refusal(&c, proof).unwrap().as_deref(), Some(TOO_MANY));
    let lock = Change::Lock { name: "ci".into() };
    super::apply(&c, &lock).unwrap().unwrap();
    assert_eq!(
        refusal(&c, proof).unwrap().as_deref(),
        Some(NO_LONGER_UNLOCKED)
    );
    super::apply(&c, &unlock("ci", T + 40)).unwrap().unwrap();
    assert_eq!(refusal(&c, proof).unwrap(), None);
}

#[test]
fn the_count_reads_only_the_index_of_destructive_changes() {
    let c = memory();
    let mut q = c.prepare(&format!("EXPLAIN QUERY PLAN {MADE}")).unwrap();
    let plan: Vec<String> = q
        .query_map([1], |r| r.get(3))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert!(
        plan.iter()
            .any(|step| step.contains("USING COVERING INDEX ledger_destructive")),
        "{plan:?}"
    );
}

/// Runs `days` for project `api`'s logs as an unlocked call of `proof`
/// would, with its row, and returns the outcome.
fn cut(c: &mut rusqlite::Connection, proof: Unlocked, days: u32) -> store::Outcome {
    let change = crate::retention::Change {
        project: "api".into(),
        logs_days: Some(Setting::Days(days)),
        errors_days: None,
    };
    let what = What {
        project: Some("api".into()),
        summary: format!("logs {days} days"),
        ..What::default()
    };
    let job = Job::Recorded {
        job: Box::new(Job::SetRetention(
            change,
            By::Unlocked(proof, Defaults::default()),
        )),
        entry: Entry::of(Who::Token("ci".into()), "set_retention", what).destructive(),
    };
    let (done, _) = tokio::sync::oneshot::channel();
    store::write_batch(c, &[Write { job, done }]).remove(0)
}

#[test]
fn the_writer_reads_the_unlock_again_and_marks_the_row_by_what_was_done() {
    let mut c = memory();
    let proof = unlocked(&c, "ci");
    assert_eq!(cut(&mut c, proof, 7), Ok(()));
    // A second call that waited behind the first finds 7 days kept: it
    // deletes nothing more, so its row is no destructive change.
    assert_eq!(cut(&mut c, proof, 7), Ok(()));
    let lock = Change::Lock { name: "ci".into() };
    super::apply(&c, &lock).unwrap().unwrap();
    // Admitted as destructive, refused once the token is locked, even
    // where it would delete nothing now.
    assert_eq!(cut(&mut c, proof, 7), Err(NO_LONGER_UNLOCKED.to_owned()));
    assert_eq!(cut(&mut c, proof, 1), Err(NO_LONGER_UNLOCKED.to_owned()));
    let mut q = c
        .prepare("SELECT outcome, destructive, repeats FROM ledger ORDER BY id")
        .unwrap();
    let got: Vec<(String, bool, i64)> = q
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        got,
        [
            ("done".to_owned(), true, 1),
            ("done".to_owned(), false, 1),
            ("error".to_owned(), false, 1),
            ("error".to_owned(), false, 1),
        ]
    );
}
