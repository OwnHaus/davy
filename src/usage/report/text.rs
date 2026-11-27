//! The report for a terminal: `davy usage` without `--json`.

use super::{Report, Signal, bytes_per_day, human, in_days, per_day, rates, safe};
use crate::{
    quota::{
        Spike,
        status::{Status, percent},
    },
    usage::{disk::Disk, forecast::Forecast},
};

impl Report {
    /// The report for a terminal.
    pub fn text(&self, out: &mut impl std::io::Write) -> std::io::Result<()> {
        let window = format!("since {} ({:.1} days)", self.since, self.days);
        writeln!(out, "{:<9} {window}", "window")?;
        match &self.disk {
            Ok(d) => Self::disk_text(d, out)?,
            Err(e) => writeln!(out, "{:<9} not measured: {}", "disk", safe(e))?,
        }
        if let Ok(f) = &self.forecast {
            writeln!(
                out,
                "{:<9} {}, steady state {}",
                "growth",
                rates(f),
                human(f.steady_state)
            )?;
        }
        writeln!(out, "{:<9} {}", "forecast", safe(&self.summary()))?;
        if let Some(e) = self.forecast.as_ref().ok().and_then(estimate) {
            writeln!(out, "{:<9} {e}", "estimate")?;
        }
        self.basis_text(out)?;
        if let Some(n) = self.default_log_days {
            writeln!(
                out,
                "{:<9} {n} days of logs for projects without their own; give --log-retention-days as davy serve has it",
                "assuming"
            )?;
        }
        self.table(out)
    }

    /// The basis and its confidence, and the range of days when the low
    /// and the high growth differ.
    fn basis_text(&self, out: &mut impl std::io::Write) -> std::io::Result<()> {
        let Some((f, l)) = self.logs() else {
            return Ok(());
        };
        let partial = self.partial();
        writeln!(
            out,
            "{:<9} {}, {} confidence: {}",
            "basis",
            l.basis.name(),
            f.confidence(partial)
                .unwrap_or(l.confidence(partial))
                .name(),
            safe(&f.explain(partial).unwrap_or_default())
        )?;
        if let Some(e) = f.errors {
            let s = if e.projects == 1 { "" } else { "s" };
            writeln!(
                out,
                "{:<9} {} events a day from {} project{s} at {} B each in SQLite, {}: bytes per event measured on {} of 7 days of SQLite growth here; the bound takes {} B each, {} a day",
                "errors",
                e.events_per_day,
                e.projects,
                e.bytes_per_event,
                e.basis.name(),
                e.own_days,
                e.growth / e.events_per_day.max(1),
                human(e.growth)
            )?;
        }
        let earliest = if f.worst_case() {
            " at the earliest"
        } else {
            ""
        };
        let range = match f.full_on_range {
            [early, late] if early == late => return Ok(()),
            [Some(early), Some(late)] => format!("full{earliest} between {early} and {late}"),
            [Some(early), None] => {
                format!("full on {early} at the earliest, not within 10 years at the low end")
            }
            [None, _] => return Ok(()),
        };
        let (high, low, what) = match f.errors {
            Some(e) if f.errors_only() => (e.growth, e.estimate_growth, "errors"),
            _ => (l.growth.high, l.growth.low, "logs"),
        };
        writeln!(
            out,
            "{:<9} {range}, at {} to {} of {what} a day",
            "range",
            human(high),
            human(low)
        )
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
        let cold = match d.cold_bytes() {
            0 => String::new(),
            b => format!(", cold {}", human(b)),
        };
        writeln!(
            out,
            "{:<9} logs {} (today {}{cold}), sqlite {}, uploads {}, other {}",
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
                safe(&g.skipped.join(", "))
            )?;
        }
        if let Some(why) = &g.stopped {
            writeln!(
                out,
                "{:<9} the walk stopped early, {}",
                "partial",
                safe(why)
            )?;
        }
        Ok(())
    }

    fn table(&self, out: &mut impl std::io::Write) -> std::io::Result<()> {
        writeln!(
            out,
            "\n{:<20} {:<11} {:>10} {:>10} {:>12} {:>10}",
            "project", "signal", "records", "bytes", "records/day", "bytes/day"
        )?;
        for p in &self.projects {
            let names = Signal::ALL
                .iter()
                .map(|s| s.name())
                .chain(["logs_stored", "logs_filtered"]);
            let counts = p.counts.iter().chain([&p.stored, &p.filtered]);
            for (name, n) in names.zip(counts) {
                writeln!(
                    out,
                    "{:<20} {name:<11} {:>10} {:>10} {:>12} {:>10}",
                    p.slug,
                    n.records,
                    human(n.bytes),
                    per_day(n.records, self.days),
                    human(bytes_per_day(n.bytes, self.days)),
                )?;
            }
            let discarded = match p.discarded {
                0 => String::new(),
                n => format!(", discarded {n} error events received"),
            };
            writeln!(
                out,
                "{:<20} keeps logs {} days, errors {} days{discarded}",
                p.slug, p.logs_days, p.errors_days
            )?;
            if let Some(line) = limits(&p.limits) {
                writeln!(out, "{:<20} {line}", p.slug)?;
            }
        }
        if self.busy_no_project > 0 {
            writeln!(
                out,
                "{:<20} refused today {} busy requests before their key was read",
                "no project", self.busy_no_project
            )?;
        }
        Ok(())
    }
}

/// A project's quotas against today's use, its spike rates and today's
/// refusals in a line, or nothing for a project with the defaults that
/// refused nothing today.
fn limits(s: &Status) -> Option<String> {
    let set = s.settings;
    let quiet = s.refused == [0, 0] && s.requests == [0, 0] && s.busy == 0;
    let spikes = [set.spike, set.spike_logs];
    if !set.has_quota() && spikes == [Spike::Default; 2] && quiet {
        return None;
    }
    let mut parts: Vec<String> = [(0, "errors"), (1, "logs")]
        .into_iter()
        .filter_map(|(i, name)| {
            let quota = set.cap(i)?;
            let shown = |n: u64| if i == 0 { n.to_string() } else { human(n) };
            Some(format!(
                "{name} {} of {} today ({}%)",
                shown(s.today[i]),
                shown(quota),
                percent(s.today[i], quota)
            ))
        })
        .collect();
    let spike = |i: usize| match (set.rate(i), spikes[i]) {
        (None, _) => "off".to_owned(),
        (Some(n), Spike::Default) => format!("{n}/s (default)"),
        (Some(n), _) => format!("{n}/s"),
    };
    parts.push(format!("spike errors {}, logs {}", spike(0), spike(1)));
    let [quota, spike] = s.refused;
    parts.push(format!(
        "refused today {quota} over quota, {spike} over spike"
    ));
    if let [quota, spike] = s.requests
        && quota + spike > 0
    {
        parts.push(format!(
            "log requests {quota} over quota, {spike} over spike"
        ));
    }
    if s.busy > 0 {
        parts.push(format!("{} busy requests", s.busy));
    }
    Some(parts.join(", "))
}

/// The estimate in a line, if there is one.
fn estimate(f: &Forecast) -> Option<String> {
    let e = f.estimate?;
    let when = match (e.fill.days_left, e.fill.full_on) {
        _ if e.fill.beyond_10_years => "full beyond 10 years".to_owned(),
        (Some(n), Some(on)) => format!("full on {on}, {}", in_days(n)),
        _ => "never full at the current rate".to_owned(),
    };
    let clamped = if e.clamped {
        ", the bound's day: its own sums put the disk full earlier, as its ratio puts less on disk than the walk found"
    } else {
        ""
    };
    Some(format!(
        "{when}{clamped}, steady state {}, {}",
        human(e.steady_state),
        f.estimate_takes()
    ))
}
