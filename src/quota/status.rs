//! Each project's quotas against what it sent today, and its spike
//! rates, as `davy project quota --json`, `davy usage --json`, the web UI
//! and the MCP tool `usage` show it. Today's use is what the writer
//! stored: the counts of the last few seconds wait in memory for the next
//! flush, so a server may refuse a few seconds before `refusing` says so.

use super::{BUSY, QUOTA, SPIKE, Settings, Spike, settings};
use crate::{time, usage};
use anyhow::Result;
use rusqlite::Connection;
use serde_json::{Value, json};
use std::collections::HashMap;

/// The reasons a status counts refusals of, in the order of
/// [`Status::refused`].
pub const REASONS: [&str; 2] = [QUOTA, SPIKE];
/// Why each of [`REASONS`] refused, for a person.
const WHY: [&str; 2] = ["past a quota", "past a spike rate"];

/// One project's quotas and its use today.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Status {
    pub settings: Settings,
    /// Error events and log bytes stored today, in the order of
    /// [`super::capped`].
    pub today: [u64; 2],
    /// Items refused today, in the order of [`REASONS`].
    pub refused: [u64; 2],
    /// Log requests refused today before their body was read, in the
    /// order of [`REASONS`].
    pub requests: [u64; 2],
    /// Requests refused today with a `429` because the body budget or a
    /// queue was full, whatever they carried.
    pub busy: u64,
}

impl Status {
    /// The status as JSON at `now_ns`, numbers only: events for errors,
    /// bytes for logs.
    pub fn json(&self, now_ns: i64) -> Value {
        let signal = |i: usize| {
            self.settings.cap(i).map(|quota| {
                json!({
                    "quota": quota,
                    "today": self.today[i],
                    "percent": percent(self.today[i], quota),
                    "refusing": self.refusing(i),
                })
            })
        };
        let by_reason = |counts: [u64; 2]| -> serde_json::Map<String, Value> {
            REASONS
                .iter()
                .zip(counts)
                .map(|(reason, n)| ((*reason).to_owned(), json!(n)))
                .collect()
        };
        json!({
            "errors": signal(0),
            "logs": signal(1),
            "warn_percent": self.settings.warn,
            "spike": {
                "errors": spike(self.settings.spike, self.settings.rate(0)),
                "logs": spike(self.settings.spike_logs, self.settings.rate(1)),
            },
            "resets_at": time::rfc3339(time::next_midnight(now_ns)),
            "refused_today": by_reason(self.refused),
            "refused_log_requests_today": by_reason(self.requests),
            "busy_refused_requests_today": self.busy,
        })
    }

    /// Whether ingest refuses the signal in place `i` of
    /// [`super::capped`] past its quota, by what is stored today.
    pub fn refusing(&self, i: usize) -> bool {
        self.settings.cap(i).is_some_and(|cap| self.today[i] >= cap)
    }

    /// The status as lines for a person, sizes in powers of 1000.
    pub fn text(&self, now_ns: i64) -> String {
        let mut lines = Vec::new();
        for (i, name) in ["errors", "logs"].into_iter().enumerate() {
            let shown = |n: u64| {
                if i == 0 {
                    n.to_string()
                } else {
                    usage::human(n)
                }
            };
            lines.push(match self.settings.cap(i) {
                None => format!("{name}: no quota"),
                Some(cap) => format!(
                    "{name}: {} of {} today ({}%){}",
                    shown(self.today[i]),
                    shown(cap),
                    percent(self.today[i], cap),
                    if self.refusing(i) { ", refusing" } else { "" },
                ),
            });
        }
        lines.push(format!("warn at: {}%", self.settings.warn));
        for (i, name) in ["errors", "logs"].into_iter().enumerate() {
            let setting = [self.settings.spike, self.settings.spike_logs][i];
            lines.push(match (self.settings.rate(i), setting) {
                (None, _) => format!("spike {name}: off"),
                (Some(n), Spike::Default) => format!("spike {name}: {n} a second, the default"),
                (Some(n), _) => format!("spike {name}: {n} a second"),
            });
        }
        lines.push(format!(
            "resets at: {}",
            time::rfc3339(time::next_midnight(now_ns))
        ));
        for (i, why) in WHY.iter().enumerate() {
            lines.push(format!(
                "refused today {why}: {} items, {} log requests",
                self.refused[i], self.requests[i]
            ));
        }
        lines.push(format!("refused today while busy: {} requests", self.busy));
        lines.push(String::new());
        lines.join("\n")
    }
}

/// A spike setting as JSON: `default`, `off` or `set`, and the rate it
/// gives, `null` for none.
fn spike(setting: Spike, rate: Option<u64>) -> Value {
    let setting = if rate.is_none() {
        "off"
    } else {
        setting.name()
    };
    json!({"setting": setting, "per_second": rate})
}

/// `used` in whole percent of `quota`, rounded down.
pub fn percent(used: u64, quota: u64) -> u64 {
    let p = u128::from(used) * 100 / u128::from(quota.max(1));
    u64::try_from(p).unwrap_or(u64::MAX)
}

/// The status of every project with a setting other than the default or
/// a refusal today, keyed by project. A project not in it has the
/// defaults and refused nothing.
pub fn all(c: &Connection, now_ns: i64) -> Result<HashMap<i64, Status>> {
    let mut out: HashMap<i64, Status> = settings::all(c)?
        .into_iter()
        .map(|(id, settings)| {
            let status = Status {
                settings,
                ..Status::default()
            };
            (id, status)
        })
        .collect();
    let today = time::day_of(now_ns);
    for ((project, signal), count) in usage::between(c, today, today, None)? {
        if let (Some(status), Some(i)) = (out.get_mut(&project), super::capped(signal)) {
            status.today[i] = if i == 0 { count.records } else { count.bytes };
        }
    }
    refused(c, now_ns, &mut out)?;
    requests(c, now_ns, &mut out)?;
    Ok(out)
}

/// The status of `project`.
pub fn of(c: &Connection, project: i64, now_ns: i64) -> Result<Status> {
    let mut status = all(c, now_ns)?.remove(&project).unwrap_or_default();
    status.settings = settings::of(c, project)?;
    Ok(status)
}

/// Adds today's refusals per reason to `out`.
fn refused(c: &Connection, now_ns: i64, out: &mut HashMap<i64, Status>) -> Result<()> {
    let day_ms = time::midnight(time::day_of(now_ns)) / 1_000_000;
    let mut q = c.prepare_cached(
        "SELECT project_id, reason, SUM(quantity) FROM client_reports
         WHERE day=?1 AND reason IN (SELECT value FROM json_each(?2))
         GROUP BY project_id, reason",
    )?;
    let reasons = serde_json::to_string(&REASONS)?;
    let rows = q.query_map((day_ms, reasons), |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
        ))
    })?;
    for row in rows {
        let (project, reason, n) = row?;
        if let Some(i) = REASONS.iter().position(|r| *r == reason) {
            out.entry(project).or_default().refused[i] = u64::try_from(n).unwrap_or(0);
        }
    }
    Ok(())
}

/// Adds today's log requests refused unread per reason, and the requests
/// refused while busy, to `out`.
fn requests(c: &Connection, now_ns: i64, out: &mut HashMap<i64, Status>) -> Result<()> {
    let day_ms = time::midnight(time::day_of(now_ns)) / 1_000_000;
    let mut q =
        c.prepare_cached("SELECT project_id, reason, quantity FROM refused_requests WHERE day=?1")?;
    let rows = q.query_map([day_ms], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
        ))
    })?;
    for row in rows {
        let (project, reason, n) = row?;
        let n = u64::try_from(n).unwrap_or(0);
        if reason == BUSY {
            out.entry(project).or_default().busy = n;
        } else if let Some(i) = REASONS.iter().position(|r| *r == reason) {
            out.entry(project).or_default().requests[i] = n;
        }
    }
    Ok(())
}
