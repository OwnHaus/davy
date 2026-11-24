//! The answer of the `usage` tool and `davy usage`: counts over a window,
//! what is on disk and the forecast.

use super::{
    Count, Signal,
    disk::{self, Disk},
    forecast::{self, Forecast, HEADROOM_REASON, WhatIf},
};
use crate::{retention, time};
use anyhow::{Result, bail};
use jiff::civil::Date;
use rusqlite::Connection;
use serde_json::{Map, Value, json};
use std::{collections::HashMap, path::Path};

/// How far back `since` may reach.
pub const MAX_SINCE_DAYS: i64 = 90;
const DAY_NS: i64 = 86_400_000_000_000;

pub struct Request {
    pub project: Option<String>,
    /// `7d` when not given.
    pub since: Option<String>,
    pub what_if: WhatIf,
    pub defaults: retention::Defaults,
}

pub struct Report {
    /// The first day of the window, which ends now.
    pub since: Date,
    /// The window's length in days, at least 1 second's worth.
    pub days: f64,
    pub disk: Disk,
    pub forecast: Result<Forecast, String>,
    pub what_if: WhatIf,
    pub projects: Vec<Project>,
}

pub struct Project {
    pub slug: String,
    pub logs_days: u32,
    pub errors_days: u32,
    pub counts: [Count; 5],
}

pub fn report(c: &Connection, data: &Path, req: &Request, now_ns: i64) -> Result<Report> {
    let since =
        time::instant(req.since.as_deref().unwrap_or("7d"), now_ns).map_err(anyhow::Error::msg)?;
    if since < now_ns.saturating_sub(MAX_SINCE_DAYS * DAY_NS) {
        bail!("since reaches back at most {MAX_SINCE_DAYS}d");
    }
    let from = time::day_of(since.min(now_ns));
    // Midnight UTC of the first day.
    let start = since.min(now_ns).div_euclid(DAY_NS) * DAY_NS;
    let today = time::day_of(now_ns);
    let disk = disk::measure(data, today)?;
    let ids = ids(c)?;
    let kept = retention::all(c, req.defaults, now_ns / 1_000_000)?;
    let filter = match &req.project {
        Some(slug) if !ids.contains_key(slug) => bail!(retention::unknown(slug)),
        Some(slug) => Some(ids[slug]),
        None => None,
    };
    let counted = super::since(c, from, filter)?;
    let forecast = forecast::forecast(&input(c, &disk, &ids, &kept, req, today)?, &req.what_if);
    let projects = kept
        .iter()
        .filter(|k| req.project.as_ref().is_none_or(|s| *s == k.project))
        .map(|k| Project {
            slug: k.project.clone(),
            logs_days: k.logs.days.get(),
            errors_days: k.errors.days.get(),
            counts: Signal::ALL.map(|s| {
                counted
                    .get(&(ids[&k.project], s))
                    .copied()
                    .unwrap_or_default()
            }),
        })
        .collect();
    Ok(Report {
        since: from,
        days: days(now_ns.saturating_sub(start)),
        disk,
        forecast,
        what_if: req.what_if.clone(),
        projects,
    })
}

/// What the forecast reads: the newest closed log days, and each project's
/// share of the log bytes counted on those days.
fn input(
    c: &Connection,
    disk: &Disk,
    ids: &HashMap<String, i64>,
    kept: &[retention::Retention],
    req: &Request,
    today: Date,
) -> Result<forecast::Input> {
    let newest = &disk.closed[disk.closed.len().saturating_sub(forecast::DAYS)..];
    let shares = match (newest.first(), newest.last()) {
        (Some((from, _)), Some((to, _))) => super::log_bytes(c, *from, *to)?,
        _ => HashMap::new(),
    };
    Ok(forecast::Input {
        today,
        closed: newest.iter().map(|(_, b)| *b).collect(),
        davy: disk.davy(),
        fixed: disk.fixed(),
        used: disk.used(),
        total: disk.total,
        projects: kept
            .iter()
            .map(|k| forecast::Project {
                slug: k.project.clone(),
                log_bytes: shares.get(&ids[&k.project]).copied().unwrap_or(0),
                logs_days: k.logs.days.get(),
            })
            .collect(),
        default_days: req.defaults.logs.get(),
    })
}

fn ids(c: &Connection) -> Result<HashMap<String, i64>> {
    let mut q = c.prepare("SELECT slug, id FROM projects")?;
    let rows = q.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// `ns` in days, at least one second's worth so a rate never divides by 0.
fn days(ns: i64) -> f64 {
    let secs = u32::try_from((ns / 1_000_000_000).max(1)).unwrap_or(u32::MAX);
    f64::from(secs) / 86_400.0
}

/// `n` over `days`, to one decimal.
#[expect(
    clippy::cast_precision_loss,
    reason = "a rate to one decimal does not need the low bits of a count over 2^53"
)]
fn per_day(n: u64, days: f64) -> f64 {
    (n as f64 / days * 10.0).round() / 10.0
}

/// `bytes` in powers of 1000 with one decimal, such as `4.1 GB`.
pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut unit = 0;
    let mut scale = 1_u128;
    while unit + 1 < UNITS.len() && u128::from(bytes) >= scale * 1000 {
        unit += 1;
        scale *= 1000;
    }
    if unit == 0 {
        return format!("{bytes} B");
    }
    let tenths = (u128::from(bytes) * 10 + scale / 2) / scale;
    format!("{}.{} {}", tenths / 10, tenths % 10, UNITS[unit])
}

impl Report {
    /// Sizes as [`human`] strings, or with `human` false as bytes.
    pub fn json(&self, human: bool) -> Value {
        let size = |b: u64| {
            if human {
                json!(self::human(b))
            } else {
                json!(b)
            }
        };
        let f = self.forecast.as_ref().ok();
        let d = &self.disk;
        let mut out = json!({
            "window": {"since": self.since.to_string(), "days": (self.days * 10.0).round() / 10.0},
            "disk": {
                "used": size(d.used()),
                "free": size(d.free),
                "total": size(d.total),
                "headroom": size(d.total / 100 * forecast::HEADROOM_PERCENT),
                "headroom_reason": HEADROOM_REASON,
                "steady_state": f.map(|f| size(f.steady_state)),
                "days_left": f.and_then(|f| f.days_left),
                "full_on": f.and_then(|f| f.full_on).map(|d| d.to_string()),
            },
            "davy": {
                "logs": size(d.logs()),
                "logs_today": size(d.today),
                "sqlite": size(d.sqlite),
                "uploads": size(d.uploads),
                "other": size(d.other),
            },
            "growth_per_day": f.map(|f| size(f.growth)),
            "forecast": self.summary(),
            "projects": self.projects.iter().map(|p| self.project(p, &size)).collect::<Vec<_>>(),
        });
        if let Err(reason) = &self.forecast {
            out["no_forecast"] = json!(reason);
        }
        if !self.what_if.is_empty() {
            out["what_if"] = json!(self.what_if);
        }
        out
    }

    fn project(&self, p: &Project, size: &impl Fn(u64) -> Value) -> Value {
        let signals: Map<String, Value> = Signal::ALL
            .iter()
            .zip(p.counts)
            .map(|(s, n)| {
                let v = json!({
                    "records": n.records,
                    "bytes": size(n.bytes),
                    "records_per_day": per_day(n.records, self.days),
                    "bytes_per_day": size(n.bytes.saturating_mul(10) / self.tenths_of_days()),
                });
                (s.name().to_owned(), v)
            })
            .collect();
        let growth = self.forecast.as_ref().ok().and_then(|f| {
            f.per_project
                .iter()
                .find(|g| g.project.as_ref() == Some(&p.slug))
                .map(|g| size(g.bytes_per_day))
        });
        json!({
            "project": p.slug,
            "logs_days": p.logs_days,
            "errors_days": p.errors_days,
            "log_growth_per_day": growth,
            "signals": signals,
        })
    }

    /// The window in tenths of a day, at least 1, to divide bytes by.
    fn tenths_of_days(&self) -> u64 {
        let tenths = (self.days * 10.0).round().max(1.0);
        time::whole(tenths)
            .and_then(|n| u64::try_from(n).ok())
            .unwrap_or(1)
    }

    /// One sentence on when the disk fills.
    pub fn summary(&self) -> String {
        match &self.forecast {
            Err(reason) => format!("no forecast: {reason}"),
            Ok(f) => match (f.days_left, f.full_on) {
                (Some(0), _) => "the disk is past total minus headroom now".to_owned(),
                (Some(n), Some(on)) => format!(
                    "full on {on}, in {n} days, at {} of logs a day",
                    human(f.growth)
                ),
                _ => format!(
                    "never fills at the current rate: retention holds Davy at {}",
                    human(f.steady_state)
                ),
            },
        }
    }

    /// The report for a terminal.
    pub fn text(&self, out: &mut impl std::io::Write) -> std::io::Result<()> {
        let d = &self.disk;
        let window = format!("since {} ({:.1} days)", self.since, self.days);
        writeln!(out, "{:<9} {window}", "window")?;
        writeln!(
            out,
            "{:<9} {} used of {}, {} free",
            "disk",
            human(d.used()),
            human(d.total),
            human(d.free)
        )?;
        writeln!(
            out,
            "{:<9} logs {} (today {}), sqlite {}, uploads {}, other {}",
            "davy",
            human(d.logs()),
            human(d.today),
            human(d.sqlite),
            human(d.uploads),
            human(d.other)
        )?;
        if let Ok(f) = &self.forecast {
            writeln!(
                out,
                "{:<9} {} of logs a day, steady state {}",
                "growth",
                human(f.growth),
                human(f.steady_state)
            )?;
        }
        writeln!(out, "{:<9} {}", "forecast", self.summary())?;
        self.table(out)
    }

    fn table(&self, out: &mut impl std::io::Write) -> std::io::Result<()> {
        writeln!(
            out,
            "\n{:<20} {:<9} {:>10} {:>10} {:>12} {:>10}",
            "project", "signal", "records", "bytes", "records/day", "bytes/day"
        )?;
        for p in &self.projects {
            for (s, n) in Signal::ALL.iter().zip(p.counts) {
                writeln!(
                    out,
                    "{:<20} {:<9} {:>10} {:>10} {:>12} {:>10}",
                    p.slug,
                    s.name(),
                    n.records,
                    human(n.bytes),
                    per_day(n.records, self.days),
                    human(n.bytes.saturating_mul(10) / self.tenths_of_days()),
                )?;
            }
            writeln!(
                out,
                "{:<20} keeps logs {} days, errors {} days",
                p.slug, p.logs_days, p.errors_days
            )?;
        }
        Ok(())
    }
}
