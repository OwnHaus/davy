//! The payload reads of a store before and after `latest_payloads`.
use super::{out, ratio};
use anyhow::{Result, bail};
use davy::store;
use rusqlite::{Connection, OpenFlags, params};
use std::{path::Path, time::Instant};

/// The reads of `src/issues.rs` before `latest_payloads`.
const OLD_STORED: &str = "SELECT event_id,payload FROM events WHERE issue_id=?1 AND payload IS NOT NULL AND (?2 IS NULL OR event_id=?2) ORDER BY id DESC LIMIT 1";
const OLD_PAGE: &str = "SELECT id,event_id,timestamp,release,environment,payload IS NOT NULL FROM events WHERE issue_id=?1 AND id<?2 ORDER BY id DESC LIMIT ?3";
/// The reads of `src/issues.rs` now.
const NEW_STORED: &str = "SELECT e.event_id,coalesce(e.payload,l.payload) FROM events e LEFT JOIN latest_payloads l ON l.issue_id=?1 AND l.event=e.id WHERE e.issue_id=?1 AND (e.payload IS NOT NULL OR l.event IS NOT NULL) AND (?2 IS NULL OR e.event_id=?2) ORDER BY e.id DESC LIMIT 1";
const NEW_PAGE: &str = "SELECT id,event_id,timestamp,release,environment,payload IS NOT NULL OR id IS (SELECT event FROM latest_payloads WHERE issue_id=?1) FROM events WHERE issue_id=?1 AND id<?2 ORDER BY id DESC LIMIT ?3";
/// Calls per issue in one round.
const REPS: usize = 50;

struct Store {
    c: Connection,
    issues: Vec<i64>,
}

impl Store {
    fn open(data: &str) -> Result<Self> {
        let c = Connection::open_with_flags(
            Path::new(data).join(store::DB_FILE),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let issues = c
            .prepare("SELECT id FROM issues")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(Self { c, issues })
    }

    /// Microseconds per call of `sql` over every issue, prepared for each
    /// call, as `src/issues.rs` does.
    fn time(&self, sql: &str, page: bool) -> Result<f64> {
        let started = Instant::now();
        for _ in 0..REPS {
            for &issue in &self.issues {
                let mut q = self.c.prepare(sql)?;
                let mut rows = if page {
                    q.query(params![issue, i64::MAX, 51])?
                } else {
                    q.query(params![issue, None::<String>])?
                };
                while let Some(r) = rows.next()? {
                    let _: Option<Vec<u8>> = r.get(1).ok();
                }
            }
        }
        Ok(started.elapsed().as_secs_f64() * 1e6 / ratio(REPS * self.issues.len()))
    }
}

pub fn compare(args: &[String]) -> Result<()> {
    let [old, new, rounds] = args else {
        bail!("expected OLD NEW ROUNDS");
    };
    let (old, new) = (Store::open(old)?, Store::open(new)?);
    let cases: [(&str, &Store, &str, bool); 4] = [
        ("old_stored", &old, OLD_STORED, false),
        ("new_stored", &new, NEW_STORED, false),
        ("old_page", &old, OLD_PAGE, true),
        ("new_page", &new, NEW_PAGE, true),
    ];
    let mut times = vec![Vec::new(); cases.len()];
    for _ in 0..rounds.parse::<usize>()? {
        for (case, t) in cases.iter().zip(&mut times) {
            t.push(case.1.time(case.2, case.3)?);
        }
    }
    let latest: i64 = new
        .c
        .query_row("SELECT count(*) FROM latest_payloads", [], |r| r.get(0))?;
    out("new_latest_payloads", latest)?;
    for (case, mut t) in cases.iter().zip(times) {
        t.sort_by(f64::total_cmp);
        out(&format!("{}_us", case.0), format!("{:.1}", t[t.len() / 2]))?;
    }
    Ok(())
}
