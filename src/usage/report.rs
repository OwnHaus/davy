//! The answer of the `usage` tool and `davy usage`: counts over a window,
//! what is on disk and the forecast.

use super::{
    Count, Signal,
    disk::{self, Disk},
    forecast::{self, Forecast, HEADROOM_REASON, NoForecast, WhatIf},
};
use crate::{retention, time};
use anyhow::{Result, anyhow, bail};
use jiff::civil::Date;
use rusqlite::Connection;
use serde_json::{Map, Value, json};
use std::path::Path;

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
    /// The first day of the window, which ends now. Counts are kept per
    /// UTC day, so the window starts at midnight UTC of the day `since`
    /// names.
    pub since: Date,
    /// The window's length in days, at least 1 second's worth.
    pub days: f64,
    /// What is on disk, or why the walk failed. Counts come back either
    /// way.
    pub disk: Result<Disk, String>,
    pub forecast: Result<Forecast, NoForecast>,
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
    if since > now_ns {
        bail!("since is in the future");
    }
    if since < now_ns.saturating_sub(MAX_SINCE_DAYS * DAY_NS) {
        bail!("since reaches back at most {MAX_SINCE_DAYS}d");
    }
    let from = time::day_of(since);
    let today = time::day_of(now_ns);
    // One statement reads every project with its id and retention, so a
    // project created meanwhile is either in all of the report or in none.
    let kept = retention::all_by_id(c, req.defaults, now_ns / 1_000_000)?;
    req.what_if
        .check(kept.iter().map(|(_, k)| k.project.as_str()))
        .map_err(anyhow::Error::msg)?;
    let filter = match &req.project {
        Some(slug) => Some(
            kept.iter()
                .find(|(_, k)| k.project == *slug)
                .map(|(id, _)| *id)
                .ok_or_else(|| anyhow!(retention::unknown(slug)))?,
        ),
        None => None,
    };
    let counted = super::between(c, from, today, filter)?;
    let disk = disk::measure(data, today).map_err(|e| format!("{e:#}"));
    let forecast = match &disk {
        Err(e) => Err(NoForecast::Other(format!("the disk was not measured: {e}"))),
        Ok(d) => match d.logs_unmeasured() {
            Some(why) => Err(NoForecast::Other(why)),
            None => forecast::forecast(&input(d, &kept, req, today), &req.what_if),
        },
    };
    let projects = kept
        .iter()
        .filter(|(id, _)| filter.is_none_or(|f| f == *id))
        .map(|(id, k)| Project {
            slug: k.project.clone(),
            logs_days: k.logs.days.get(),
            errors_days: k.errors.days.get(),
            counts: Signal::ALL.map(|s| counted.get(&(*id, s)).copied().unwrap_or_default()),
        })
        .collect();
    // Midnight UTC of the first day.
    let start = since.div_euclid(DAY_NS) * DAY_NS;
    Ok(Report {
        since: from,
        days: days(now_ns.saturating_sub(start)),
        disk,
        forecast,
        what_if: req.what_if.clone(),
        projects,
    })
}

/// What the forecast reads: the closed log days and each project's log
/// retention.
fn input(
    disk: &Disk,
    kept: &[(i64, retention::Retention)],
    req: &Request,
    today: Date,
) -> forecast::Input {
    forecast::Input {
        today,
        closed: disk.closed.clone(),
        davy: disk.davy(),
        fixed: disk.fixed(),
        used: disk.used(),
        total: disk.total,
        projects: kept
            .iter()
            .map(|(_, k)| (k.project.clone(), k.logs.days.get()))
            .collect(),
        default_days: req.defaults.logs.get(),
    }
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

/// `bytes` over `days`, in whole bytes.
fn bytes_per_day(bytes: u64, days: f64) -> u64 {
    time::whole(per_day(bytes, days).round())
        .and_then(|n| u64::try_from(n).ok())
        .unwrap_or(u64::MAX)
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
        let mut out = json!({
            "window": {"since": self.since.to_string(), "days": (self.days * 10.0).round() / 10.0},
            "growth_per_day": f.map(|f| size(f.growth)),
            "longest_log_days": f.map(|f| f.longest_days),
            "forecast": self.summary(),
            "projects": self.projects.iter().map(|p| self.project(p, &size)).collect::<Vec<_>>(),
        });
        match &self.disk {
            Ok(d) => {
                out["disk"] = self.disk_json(d, &size);
                out["davy"] = json!({
                    "logs": size(d.logs()),
                    "logs_today": size(d.today),
                    "sqlite": size(d.sqlite),
                    "uploads": size(d.uploads),
                    "other": size(d.other),
                });
            }
            Err(e) => out["disk_error"] = json!(e),
        }
        if let Err(reason) = &self.forecast {
            out["no_forecast"] = json!(reason.to_string());
        }
        if !self.what_if.is_empty() {
            out["what_if"] = json!(self.what_if);
        }
        out
    }

    fn disk_json(&self, d: &Disk, size: &impl Fn(u64) -> Value) -> Value {
        let f = self.forecast.as_ref().ok();
        let g = &d.gaps;
        json!({
            "used": size(d.used()),
            "free": size(d.free),
            "total": size(d.total),
            "headroom": size(d.total / 100 * forecast::HEADROOM_PERCENT),
            "headroom_reason": HEADROOM_REASON,
            "steady_state": f.map(|f| size(f.steady_state)),
            "days_left": f.and_then(|f| f.days_left),
            "full_on": f.and_then(|f| f.full_on).map(|d| d.to_string()),
            "partial": (!g.is_empty()).then(|| json!({
                "skipped": g.skipped,
                "skipped_count": g.skipped_count,
                "stopped": g.stopped,
            })),
        })
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
                    "bytes_per_day": size(bytes_per_day(n.bytes, self.days)),
                });
                (s.name().to_owned(), v)
            })
            .collect();
        json!({
            "project": p.slug,
            "logs_days": p.logs_days,
            "errors_days": p.errors_days,
            "signals": signals,
        })
    }

    /// One sentence on when the disk fills.
    pub fn summary(&self) -> String {
        match &self.forecast {
            Err(reason) => format!("no forecast: {reason}"),
            Ok(f) => match f.days_left {
                Some(0) => "the disk is past total minus headroom now".to_owned(),
                Some(n) => {
                    let on = f.full_on.map(|on| format!("on {on}, ")).unwrap_or_default();
                    format!("full {on}in {n} days, at {} of logs a day", human(f.growth))
                }
                None => format!(
                    "never fills at the current rate: even the longest log retention, {} days, holds Davy at {}",
                    f.longest_days,
                    human(f.steady_state)
                ),
            },
        }
    }

    /// The report for a terminal.
    pub fn text(&self, out: &mut impl std::io::Write) -> std::io::Result<()> {
        let window = format!("since {} ({:.1} days)", self.since, self.days);
        writeln!(out, "{:<9} {window}", "window")?;
        match &self.disk {
            Ok(d) => Self::disk_text(d, out)?,
            Err(e) => writeln!(out, "{:<9} not measured: {e}", "disk")?,
        }
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

    fn disk_text(d: &Disk, out: &mut impl std::io::Write) -> std::io::Result<()> {
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
        let g = &d.gaps;
        if !g.skipped.is_empty() {
            let named = u64::try_from(g.skipped.len()).unwrap_or(u64::MAX);
            let more = g.skipped_count.saturating_sub(named);
            let more = if more > 0 {
                format!(" and {more} more")
            } else {
                String::new()
            };
            writeln!(
                out,
                "{:<9} left out {}{more}",
                "partial",
                g.skipped.join(", ")
            )?;
        }
        if let Some(why) = &g.stopped {
            writeln!(out, "{:<9} the walk stopped early, {why}", "partial")?;
        }
        Ok(())
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
                    human(bytes_per_day(n.bytes, self.days)),
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
