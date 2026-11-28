//! What the ledger's upkeep costs the writer on a full ledger: 51 agent
//! actors, the admin token and 50 tokens, each with 5,000 done changes,
//! 500 refused calls and 2,000 changes that deleted data, 382,500 rows in
//! all, none old enough to age out. Times the sweep, then rows added by
//! one actor at its caps, then the sweep of 6,000 rows past the half year.
//! See "Ledger" in docs/design.md.
//!
//! cargo run --release --example ledger_sweep -- WORK
//!
//! WORK is an empty directory for the database.

use anyhow::{Result, bail};
use davy::ledger::{self, Entry, What, Who};
use std::{io::Write, path::Path, time::Instant};

const TOKENS: i64 = 50;

fn out(line: &str) -> Result<()> {
    writeln!(std::io::stdout(), "{line}")?;
    Ok(())
}

/// Fills the ledger, rows of all actors interleaved as they would arrive,
/// each row of a capped group numbered as `ledger::add` would number it.
fn fill(c: &rusqlite::Connection, now: i64) -> Result<()> {
    c.execute_batch("BEGIN")?;
    for (outcome, destructive, n) in [
        ("done", 0, ledger::KEEP_DONE),
        ("denied", 0, ledger::KEEP_REFUSED),
        ("done", 1, davy::tokens::MAX_DESTRUCTIVE),
    ] {
        c.execute(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<?1),
             a(j) AS (SELECT 0 UNION ALL SELECT j+1 FROM a WHERE j<?2)
             INSERT INTO ledger(at, actor_kind, actor, action, project, target, outcome, summary, destructive, seq)
             SELECT ?3+i, CASE j WHEN 0 THEN 'admin' ELSE 'token' END,
                    CASE j WHEN 0 THEN 'admin token' ELSE printf('agent-%02d', j) END,
                    'set_retention', 'shop', 'project shop', ?4, printf('logs %d days', i), ?5,
                    CASE ?5 WHEN 0 THEN i END
             FROM n, a ORDER BY i, j",
            rusqlite::params![n, TOKENS, now - 86_400_000, outcome, destructive],
        )?;
    }
    c.execute_batch("COMMIT")?;
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [work] = args.as_slice() else {
        bail!("usage: ledger_sweep WORK");
    };
    let c = davy::store::open(&Path::new(work).join("data"))?;
    let now = davy::time::now_ms();
    fill(&c, now)?;
    let rows: i64 = c.query_row("SELECT count(*) FROM ledger", [], |r| r.get(0))?;
    let bytes: i64 = c.query_row(
        "SELECT page_count * page_size FROM pragma_page_count, pragma_page_size",
        [],
        |r| r.get(0),
    )?;
    out(&format!("rows\t{rows}\tdatabase bytes\t{bytes}"))?;
    for round in 1..=5 {
        let start = Instant::now();
        ledger::prune(&c, now)?;
        out(&format!(
            "sweep {round}\t{:.1} ms",
            start.elapsed().as_secs_f64() * 1e3
        ))?;
    }
    for (label, outcome) in [
        ("done", ledger::Outcome::Done),
        ("refused", ledger::Outcome::Denied),
    ] {
        let count = 1_000_u32;
        let start = Instant::now();
        for i in 0..count {
            let what = What {
                project: Some("shop".into()),
                target: Some("project shop".into()),
                summary: format!("errors {i} days"),
                issue: None,
            };
            let entry = Entry::of(Who::Token("agent-01".into()), "set_retention", what)
                .ended(outcome, None);
            ledger::add(&c, &entry)?;
        }
        out(&format!(
            "add {label}\t{:.1} us/row",
            start.elapsed().as_secs_f64() * 1e6 / f64::from(count)
        ))?;
    }
    let start = Instant::now();
    ledger::prune(&c, now)?;
    out(&format!(
        "sweep after adds\t{:.1} ms",
        start.elapsed().as_secs_f64() * 1e3
    ))?;
    // A backlog past the half year, as after a server was down for days.
    c.execute(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<6000)
         INSERT INTO ledger(at, actor_kind, actor, action, outcome, summary)
         SELECT ?1+i, 'person', 'ann@example.com', 'issue_status', 'done', 'resolved' FROM n",
        [now - ledger::KEEP_MS - 10_000_000],
    )?;
    for round in 1..=2 {
        let start = Instant::now();
        ledger::prune(&c, now)?;
        let left: i64 = c.query_row(
            "SELECT count(*) FROM ledger WHERE actor_kind='person'",
            [],
            |r| r.get(0),
        )?;
        out(&format!(
            "sweep of 6000 aged rows, round {round}\t{:.1} ms\t{left} left",
            start.elapsed().as_secs_f64() * 1e3
        ))?;
    }
    Ok(())
}
