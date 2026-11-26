//! The doctor check `quotas`: each project at or past a daily quota today,
//! which ingest refuses until midnight UTC. A project near its quota is
//! left to the warn alert.

use super::{Check, Status, not_ok, ok, sh};
use crate::{
    quota::{self, status},
    usage,
};
use anyhow::Result;
use rusqlite::Connection;
use std::path::Path;

const NAME: &str = "quotas";

/// The quotas of the data directory `data`, which `d` names for a shell,
/// against what each project stored today at `now_ns`.
pub(super) fn quotas(data: &Path, d: &str, now_ns: i64) -> Check {
    if !data.join(crate::store::DB_FILE).exists() {
        return check(&[], d);
    }
    match crate::query::open(data).and_then(|c| read(&c, now_ns)) {
        Ok(projects) => check(&projects, d),
        Err(e) => not_ok(
            NAME,
            Status::Warn,
            format!("the quotas were not read: {e:#}"),
            format!("make {d} readable by the user doctor runs as"),
        ),
    }
}

/// Each project with a quota, by slug.
fn read(c: &Connection, now_ns: i64) -> Result<Vec<(String, status::Status)>> {
    let mut all = status::all(c, now_ns)?;
    let mut q = c.prepare("SELECT id, slug FROM projects ORDER BY slug")?;
    let rows = q.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
    let mut out = Vec::new();
    for row in rows {
        let (id, slug) = row?;
        if let Some(s) = all.remove(&id).filter(|s| s.settings.has_quota()) {
            out.push((slug, s));
        }
    }
    Ok(out)
}

/// A warning naming each quota reached, else how many projects have one.
fn check(projects: &[(String, status::Status)], d: &str) -> Check {
    let mut reached = Vec::new();
    let mut fix = None;
    for (slug, s) in projects {
        for (i, signal) in quota::CAPPED.into_iter().enumerate() {
            let Some(cap) = s.settings.cap(i).filter(|cap| s.today[i] >= *cap) else {
                continue;
            };
            let logs = signal == usage::Signal::Logs;
            let (flag, value) = if logs {
                ("logs", "SIZE")
            } else {
                ("errors", "N")
            };
            let shown = |n: u64| if logs { usage::human(n) } else { n.to_string() };
            reached.push(format!(
                "{slug} used {} of its {} {flag} quota today ({}%)",
                shown(s.today[i]),
                shown(cap),
                status::percent(s.today[i], cap),
            ));
            fix.get_or_insert_with(|| {
                format!(
                    "davy project quota {} --data {d} --{flag} {value}",
                    sh(slug)
                )
            });
        }
    }
    if let Some(fix) = fix {
        let detail = format!(
            "{}; ingest refuses it until midnight UTC",
            reached.join("; ")
        );
        return not_ok(NAME, Status::Warn, detail, fix);
    }
    match projects.len() {
        0 => ok(NAME, "no project has a quota"),
        1 => ok(NAME, "1 project has a quota, not reached today"),
        n => ok(
            NAME,
            format!("{n} projects have a quota, none reached today"),
        ),
    }
}

#[cfg(test)]
#[path = "doctor_quota_tests.rs"]
mod tests;
