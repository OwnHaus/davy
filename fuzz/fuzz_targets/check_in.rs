#![no_main]

use davy::monitor::{
    checkin::{Config, parse},
    schedule::{Crontab, Schedule, Zone},
};
use libfuzzer_sys::fuzz_target;
use std::time::Duration;

/// 2026-11-18T11:06:40Z, a fixed instant so a crash replays.
const AT: i64 = 1_795_000_000_000;

fuzz_target!(|data: &[u8]| {
    // A schedule is stored as columns, so it must read back as itself, and
    // its next time must be found or refused without a panic or a hang.
    if let Ok(check_in) = parse(data)
        && let Config::Valid(config) = check_in.config
    {
        let (every_s, months, cron, zone) = config.schedule.columns();
        let stored = Schedule::from_columns(every_s, months, cron, zone);
        assert_eq!(stored.as_ref(), Some(&config.schedule));
        let _ = config.schedule.due(AT, config.margin);
    }
    // Mutated JSON seldom holds a crontab that parses, so the bytes are one
    // too, in a zone with DST.
    if let Ok(text) = std::str::from_utf8(data)
        && let Some(spec) = Crontab::parse(text)
        && let Some(zone) = Zone::parse("Europe/Prague")
    {
        let _ = Schedule::Cron { spec, zone }.due(AT, Duration::from_mins(5));
    }
});
