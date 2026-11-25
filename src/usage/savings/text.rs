//! The savings view of `davy usage` in a terminal.

use super::{NOTE, Savings, days, human, retention_try};
use std::{borrow::Cow, io::Write};

/// `s` with control characters replaced: an example, a title and a slug
/// come from clients and may hold bytes that move the cursor.
fn safe(s: &str) -> Cow<'_, str> {
    crate::text::printable(s, char::REPLACEMENT_CHARACTER)
}

/// `n` searches, one in the singular.
fn searches(n: u64) -> String {
    if n == 1 {
        "1 search".to_owned()
    } else {
        format!("{n} searches")
    }
}

/// `share` in whole percent.
fn percent(share: f64) -> String {
    format!("{:.0}%", share * 100.0)
}

impl Savings {
    pub fn text(&self, out: &mut impl Write) -> std::io::Result<()> {
        let window = format!("since {} ({:.1} days)", self.since, self.days);
        writeln!(out, "{:<9} {window}", "window")?;
        if self.patterns.is_empty() && self.retention.is_empty() && self.issues.is_empty() {
            writeln!(out, "{:<9} nothing to suggest", "savings")?;
        }
        for p in &self.patterns {
            let seen = p.last_queried.as_deref().unwrap_or("never");
            let of = [p.project.as_deref(), p.service.as_deref(), p.level]
                .into_iter()
                .flatten()
                .map(safe)
                .collect::<Vec<_>>()
                .join(" ");
            writeln!(
                out,
                "{:<9} {} {} ({}), {}/day, {of}, last queried {seen}",
                "pattern",
                p.id,
                human(p.bytes),
                percent(p.share),
                human(p.per_day)
            )?;
            writeln!(out, "{:<9} {}", "", safe(&p.example))?;
            writeln!(out, "{:<9} try: search_logs pattern={}", "", p.id)?;
        }
        for f in &self.retention {
            writeln!(
                out,
                "{:<9} {} keeps {} of logs; {} in {}, the oldest read {} back",
                "retention",
                safe(&f.project),
                days(f.logs_days),
                searches(f.searches),
                days(f.history_days),
                days(f.oldest_read_days)
            )?;
            writeln!(
                out,
                "{:<9} keep {} to save {}",
                "",
                days(f.suggest),
                human(f.saves)
            )?;
            let command = self.retention_command(f);
            writeln!(out, "{:<9} a person can run: {}", "", safe(&command))?;
            writeln!(out, "{:<9} try: {}", "", safe(&retention_try(f)))?;
        }
        for i in &self.issues {
            writeln!(
                out,
                "{:<9} {} in {}: {} events, {} of its error events",
                "issue",
                i.id,
                safe(&i.project),
                i.events,
                percent(i.share)
            )?;
            writeln!(out, "{:<9} {}", "", safe(&i.title))?;
            writeln!(out, "{:<9} {}", "", safe(&self.discard_suggest(i.id)))?;
            writeln!(out, "{:<9} try: issue_context {}", "", i.id)?;
        }
        if self.partial {
            writeln!(
                out,
                "{:<9} some days of pattern counters were not read in full",
                "partial"
            )?;
        }
        writeln!(out, "{:<9} {NOTE}", "note")
    }
}
