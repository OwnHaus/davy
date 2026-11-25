//! The report for a terminal: `davy usage` without `--json`.

use super::{Report, Signal, bytes_per_day, human, in_days, per_day, rates, safe};
use crate::usage::{disk::Disk, forecast::Forecast};

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
        writeln!(
            out,
            "{:<9} {range}, at {} to {} of logs a day",
            "range",
            human(l.growth.high),
            human(l.growth.low)
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
            let names = Signal::ALL.iter().map(|s| s.name()).chain(["logs_stored"]);
            for (name, n) in names.zip(p.counts.iter().chain([&p.stored])) {
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
            writeln!(
                out,
                "{:<20} keeps logs {} days, errors {} days",
                p.slug, p.logs_days, p.errors_days
            )?;
        }
        Ok(())
    }
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
