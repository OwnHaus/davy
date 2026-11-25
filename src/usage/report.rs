//! The answer of the `usage` tool and `davy usage`: counts over a window,
//! what is on disk and the forecast.

use super::{
    Count, Signal,
    disk::{self, Disk},
    forecast::{self, Confidence, Forecast, HEADROOM_REASON, NoForecast, WhatIf},
};
use crate::{retention, time};
use anyhow::{Result, anyhow, bail};
use jiff::civil::Date;
use rusqlite::Connection;
use serde_json::{Map, Value, json};
use std::path::Path;

/// How far back `since` may reach.
pub const MAX_SINCE_DAYS: i64 = 90;

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
    /// The server's log retention, when a project keeps it for want of
    /// its own. `davy usage` and `davy doctor` assume it.
    pub default_log_days: Option<u32>,
}

pub struct Project {
    pub slug: String,
    pub logs_days: u32,
    pub errors_days: u32,
    pub counts: [Count; 5],
    /// Raw bytes of the logs stored on the index days of the window and
    /// tomorrow, as the pattern counters count them. Never subtracted from what the
    /// clients sent: the two count different bytes on different days.
    pub stored: Count,
}

/// The report on `data`, which walks the disk now.
pub fn report(c: &Connection, data: &Path, req: &Request, now_ns: i64) -> Result<Report> {
    let prepared = prepare(c, req, now_ns)?;
    let disk = disk::measure(data, prepared.today).map_err(|e| format!("{e:#}"));
    Ok(prepared.finish(disk))
}

/// What a report reads from the database, checked and counted, before the
/// disk is measured.
pub struct Prepared {
    since: Date,
    days: f64,
    /// The UTC day that is still open, the day the disk is measured for.
    pub today: Date,
    /// Every project with its id and retention.
    kept: Vec<(i64, retention::Retention)>,
    what_if: WhatIf,
    default_days: u32,
    projects: Vec<Project>,
    /// Raw bytes per index day and project of the newest days, see
    /// [`forecast::Input::raw`].
    raw: Vec<forecast::RawDay>,
    counted_since: Option<i64>,
    now: i64,
    store: Vec<forecast::StoreDay>,
    errors: Vec<forecast::ProjectErrors>,
}

/// Checks `req` and reads the counts and retentions it asks for.
pub fn prepare(c: &Connection, req: &Request, now_ns: i64) -> Result<Prepared> {
    let since =
        time::instant(req.since.as_deref().unwrap_or("7d"), now_ns).map_err(anyhow::Error::msg)?;
    if since > now_ns {
        bail!("since is in the future");
    }
    if since < now_ns.saturating_sub(MAX_SINCE_DAYS * time::DAY_NS) {
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
    // A record may be indexed up to a day ahead, see `logs::store::State::settle`.
    let stored = super::stored::between(c, from, today.tomorrow()?, filter)?;
    let projects = kept
        .iter()
        .filter(|(id, _)| filter.is_none_or(|f| f == *id))
        .map(|(id, k)| Project {
            slug: k.project.clone(),
            logs_days: k.logs.days.get(),
            errors_days: k.errors.days.get(),
            counts: Signal::ALL.map(|s| counted.get(&(*id, s)).copied().unwrap_or_default()),
            stored: stored.get(id).copied().unwrap_or_default(),
        })
        .collect();
    // The estimate counts a project's logs on disk as far back as it, or
    // the what-if, keeps them.
    let longest = kept
        .iter()
        .map(|(_, k)| k.logs.days.get())
        .chain(req.what_if.logs_days)
        .max()
        .unwrap_or(0);
    let raw = raw_days(c, today, &kept, longest)?;
    let back = |days: i64| today.checked_sub(jiff::Span::new().days(days));
    let errors = errors(c, back(7)?, &kept)?;
    // Midnight UTC of the first day.
    let start = since.div_euclid(time::DAY_NS) * time::DAY_NS;
    Ok(Prepared {
        since: from,
        days: days(now_ns.saturating_sub(start)),
        today,
        kept,
        what_if: req.what_if.clone(),
        default_days: req.defaults.logs.get(),
        projects,
        raw,
        counted_since: super::stored::counted_since(c)?,
        now: now_ns,
        // The sample before the first day of the window starts its pair.
        store: super::stored::store_days(c, back(forecast::LOOK_BACK + 1)?)?,
        errors,
    })
}

/// What each project received and keeps of errors, error events received
/// from `from` on.
fn errors(
    c: &Connection,
    from: Date,
    kept: &[(i64, retention::Retention)],
) -> Result<Vec<forecast::ProjectErrors>> {
    let daily = super::errors_since(c, from)?;
    let stored = super::stored::events(c)?;
    Ok(kept
        .iter()
        .filter_map(|(id, k)| {
            let rows: Vec<_> = daily.iter().filter(|d| d.project == *id).collect();
            let row = rows.first()?;
            Some(forecast::ProjectErrors {
                slug: k.project.clone(),
                created: row.created,
                first: row.first,
                daily: rows.iter().map(|d| (d.day, d.count)).collect(),
                stored: stored.get(id).copied().unwrap_or(0),
                days: k.errors.days.get(),
            })
        })
        .collect())
}

/// Raw bytes per index day of every project, from [`forecast::LOOK_BACK`]
/// days or `longest` days before `today` on, the more, oldest first.
fn raw_days(
    c: &Connection,
    today: Date,
    kept: &[(i64, retention::Retention)],
    longest: u32,
) -> Result<Vec<forecast::RawDay>> {
    let back = forecast::LOOK_BACK.max(i64::from(longest));
    let from = jiff::Span::new()
        .try_days(back)
        .ok()
        .and_then(|span| today.checked_sub(span).ok())
        .unwrap_or(Date::MIN);
    let mut out: Vec<forecast::RawDay> = Vec::new();
    for (day, id, bytes, cut) in super::stored::since(c, from)? {
        let Some((_, k)) = kept.iter().find(|(i, _)| *i == id) else {
            continue;
        };
        let row = (k.project.clone(), bytes, cut);
        match out.last_mut() {
            Some(last) if last.day == day => last.projects.push(row),
            _ => out.push(forecast::RawDay {
                day,
                projects: vec![row],
            }),
        }
    }
    Ok(out)
}

impl Prepared {
    /// The report with `disk`, what the walk measured or why it failed.
    pub fn finish(self, disk: Result<Disk, String>) -> Report {
        let forecast = match &disk {
            Err(e) => Err(NoForecast::Failed(format!(
                "the disk was not measured: {e}"
            ))),
            Ok(d) => match d.logs_unmeasured() {
                Some(why) => Err(why),
                None => forecast::forecast(&self.input(d), &self.what_if),
            },
        };
        let default_log_days = self
            .kept
            .iter()
            .any(|(_, k)| k.logs.server_default)
            .then_some(self.default_days);
        Report {
            since: self.since,
            days: self.days,
            disk,
            forecast,
            what_if: self.what_if,
            projects: self.projects,
            default_log_days,
        }
    }

    /// What the forecast reads: the closed log days, each project's log
    /// retention and the raw bytes counted.
    fn input(&self, disk: &Disk) -> forecast::Input {
        forecast::Input {
            today: self.today,
            closed: disk.closed.clone(),
            davy: disk.davy(),
            fixed: disk.fixed(),
            used: disk.used(),
            total: disk.total,
            projects: self
                .kept
                .iter()
                .map(|(_, k)| (k.project.clone(), k.logs.days.get()))
                .collect(),
            default_days: self.default_days,
            raw: self.raw.clone(),
            counted_since: self.counted_since,
            now: self.now,
            store: self.store.clone(),
            errors: self.errors.clone(),
            prior: super::footprint::PRIOR,
        }
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

/// `s` with control characters replaced: a directory under the data
/// directory may be named with bytes that move the cursor or retitle the
/// terminal. JSON escapes them itself.
fn safe(s: &str) -> std::borrow::Cow<'_, str> {
    crate::text::printable(s, char::REPLACEMENT_CHARACTER)
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
            "shortest_log_days": f.map(|f| f.shortest_days),
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
        out["basis"] = self.basis_json(&size);
        out["estimate"] = json!(f.and_then(|f| f.estimate).map(|e| json!({
            "full_on": e.fill.full_on.map(|d| d.to_string()),
            "days_left": e.fill.days_left,
            "steady_state": size(e.steady_state),
            "beyond_10_years": e.fill.beyond_10_years,
            "clamped": e.clamped,
        })));
        if let Err(reason) = &self.forecast {
            out["no_forecast"] = json!(reason.to_string());
        }
        if !self.what_if.is_empty() {
            out["what_if"] = json!(self.what_if);
        }
        if let Some(days) = self.default_log_days {
            out["default_log_days"] = json!(days);
        }
        if let Some(note) = f.and_then(|f| f.unseen.as_ref()) {
            out["what_if_note"] = json!(note);
        }
        out
    }

    /// Whether the disk walk left something out.
    fn partial(&self) -> bool {
        self.disk.as_ref().is_ok_and(|d| !d.gaps.is_empty())
    }

    /// What the growth rests on and how far to trust it, or null where
    /// a what-if states the growth or there is no forecast.
    fn basis_json(&self, size: &impl Fn(u64) -> Value) -> Value {
        let Some((f, l)) = self.logs() else {
            return Value::Null;
        };
        let ratio = |ppm: u64| {
            let r = f64::from(u32::try_from(ppm).unwrap_or(u32::MAX)) / 1_000_000.0;
            json!((r * 10_000.0).round() / 10_000.0)
        };
        let date = |d: Option<Date>| d.map(|d| d.to_string());
        let partial = self.partial();
        json!({
            "confidence": f.confidence(partial).map(Confidence::name),
            "full_on_range": f.full_on_range.map(date),
            "logs": {
                "basis": l.basis.name(),
                "growth_per_day": {
                    "low": size(l.growth.low),
                    "mid": size(l.growth.mid),
                    "high": size(l.growth.high),
                },
                "own_days": l.own_days,
                "possible_days": l.possible_days,
                "raw_per_day": l.raw.map(|r| size(r.mid)),
                "raw_days": l.raw_days,
                "counted_hours": l.counted_ns.map(|ns| (forecast::hours(ns) * 10.0).round() / 10.0),
                "ratio": l.ratio.map(|r| ratio(r.mid)),
                "ratio_range": l.ratio.map(|r| [ratio(r.low), ratio(r.high)]),
                "own_ratio": l.own_ratio.map(ratio),
                "footprint_ratio": ratio(l.prior.ratio.mid),
                "footprint": l.prior.source,
            },
            "errors": f.errors.map(|e| json!({
                "projects": e.projects,
                "events_per_day": e.events_per_day,
                "bytes_per_event": e.bytes_per_event,
                "own_days": e.own_days,
                "basis": e.basis.name(),
                "growth_per_day": size(e.growth),
                "estimate_growth_per_day": size(e.estimate_growth),
            })),
            "why": f.explain(partial),
        })
    }

    fn logs(&self) -> Option<(&Forecast, &forecast::Logs)> {
        let f = self.forecast.as_ref().ok()?;
        Some((f, f.logs.as_ref()?))
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
            "worst_case": f.map(Forecast::worst_case),
            "full_on": f.and_then(|f| f.full_on).map(|d| d.to_string()),
            "beyond_10_years": f.map(|f| f.beyond_10_years),
            "partial": (!g.is_empty()).then(|| json!({
                "skipped": g.skipped,
                "skipped_count": g.skipped_count,
                "stopped": g.stopped,
            })),
        })
    }

    fn project(&self, p: &Project, size: &impl Fn(u64) -> Value) -> Value {
        let count = |n: Count| {
            json!({
                "records": n.records,
                "bytes": size(n.bytes),
                "records_per_day": per_day(n.records, self.days),
                "bytes_per_day": size(bytes_per_day(n.bytes, self.days)),
            })
        };
        let signals: Map<String, Value> = Signal::ALL
            .iter()
            .zip(p.counts)
            .map(|(s, n)| (s.name().to_owned(), count(n)))
            .collect();
        json!({
            "project": p.slug,
            "logs_days": p.logs_days,
            "errors_days": p.errors_days,
            "signals": signals,
            "logs_stored": count(p.stored),
        })
    }

    /// When the disk fills, in a sentence, in a worst case why the day is
    /// the earliest, and on errors while logs are left out that it may be
    /// earlier.
    pub fn summary(&self) -> String {
        let f = match &self.forecast {
            Err(reason) => return format!("no forecast: {reason}"),
            Ok(f) => f,
        };
        let when = self.when(f);
        if f.logs_pending() && f.days_left != Some(0) {
            format!(
                "{when}. Logs are not in this forecast yet, so the disk may fill earlier once they count"
            )
        } else {
            when
        }
    }

    fn when(&self, f: &Forecast) -> String {
        let rates = rates(f);
        if f.beyond_10_years {
            return format!(
                "full beyond 10 years at {rates}, as Davy grows toward {} once every retention is full",
                human(f.steady_state)
            );
        }
        let Some(n) = f.days_left else {
            let past = self
                .disk
                .as_ref()
                .is_ok_and(|d| d.used() >= d.total.saturating_sub(f.headroom));
            let past = if past {
                ". The disk is past total minus headroom already, from files Davy does not grow, so watch its free space"
            } else {
                ""
            };
            if f.errors_only() {
                return format!(
                    "errors never fill the disk at the current rate: each project's error retention holds Davy at {}{past}",
                    human(f.steady_state)
                );
            }
            let (what, errors, holds) = match f.errors {
                Some(_) => (
                    "logs and errors",
                    " and each project's error retention",
                    "hold",
                ),
                None => ("logs", "", "holds"),
            };
            return format!(
                "{what} never fill the disk at the current rate: even the longest log retention, {} days,{errors} {holds} Davy at {}{past}",
                f.longest_days,
                human(f.steady_state)
            );
        };
        if n == 0 {
            return "the disk is past total minus headroom now".to_owned();
        }
        let on = f.full_on.map(|on| format!("on {on}, ")).unwrap_or_default();
        let n = in_days(n);
        if !f.worst_case() {
            return format!("full {on}{n}, at {rates}");
        }
        format!(
            "full {on}{n} at the earliest, at {rates}. This is a worst case: projects keep logs {} to {l} days and share one index per day, so Davy cannot tell each project's share and takes all logs as kept {l} days. At the current rate of logs, the disk fills on this day or later, or never; the estimate keeps each project's logs as long as it keeps them. Watch free space over a few days before you shorten a retention for this date",
            f.shortest_days,
            l = f.longest_days,
        )
    }
}

/// "in `n` days", or "in 1 day".
fn in_days(n: u64) -> String {
    if n == 1 {
        "in 1 day".to_owned()
    } else {
        format!("in {n} days")
    }
}

/// The growth the forecast decides on, as "up to" when that is the high
/// end of a range.
fn rate(f: &Forecast) -> String {
    let high = f.logs.as_ref().is_some_and(|l| l.growth.mid != f.growth);
    let up_to = if high { "up to " } else { "" };
    format!("{up_to}{}", human(f.growth))
}

mod text;

/// What grows a day: the logs, and the errors where projects receive
/// them.
fn rates(f: &Forecast) -> String {
    match f.errors {
        Some(e) if f.errors_only() => format!("{} of errors a day", human(e.growth)),
        Some(e) => format!(
            "{} of logs and {} of errors a day",
            rate(f),
            human(e.growth)
        ),
        None => format!("{} of logs a day", rate(f)),
    }
}

#[cfg(test)]
#[path = "report_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "report_forecast_tests.rs"]
mod forecast_tests;
