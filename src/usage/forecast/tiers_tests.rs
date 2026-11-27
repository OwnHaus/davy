use super::*;
use crate::{
    time,
    usage::{footprint::FIXTURE, forecast::RawDay},
};

const GB: u64 = 1_000_000_000;
const MB: u64 = 1_000_000;

fn day(s: &str) -> Date {
    s.parse().unwrap()
}

fn raw(d: &str, bytes: u64, cut: bool) -> RawDay {
    RawDay {
        day: day(d),
        projects: vec![("api".to_owned(), bytes, cut)],
    }
}

/// A server counting since 2026-11-10, asked on 2026-11-24 with 3 hot
/// closed days of 10 GB raw that take 6 GB each, and 3 cold days of 10 GB
/// raw that take 0.5, 0.6 and 0.7 GB.
fn server() -> Input {
    let hot = ["2026-11-21", "2026-11-22", "2026-11-23"];
    let cold = ["2026-11-18", "2026-11-19", "2026-11-20"];
    Input {
        today: day("2026-11-24"),
        closed: hot.iter().map(|d| (day(d), 6 * GB)).collect(),
        cold: cold
            .iter()
            .zip([500, 600, 700])
            .map(|(d, mb)| (day(d), mb * MB))
            .collect(),
        hot_days: Some(3),
        davy: 0,
        fixed: 0,
        used: 0,
        total: 0,
        projects: vec![("api".to_owned(), 30)],
        default_days: 30,
        raw: hot
            .iter()
            .chain(&cold)
            .map(|d| raw(d, 10 * GB, false))
            .collect(),
        counted_since: Some(time::midnight(day("2026-11-10"))),
        now: 0,
        store: Vec::new(),
        errors: Vec::new(),
        prior: FIXTURE,
    }
}

#[test]
fn the_cold_ratio_is_the_servers_own_where_it_holds_both_tiers() {
    // 0.06 of raw cold over 0.6 hot.
    let own = Tiers {
        hot_days: Some(3),
        ratio_ppm: 100_000,
        own: true,
    };
    assert_eq!(Tiers::of(&server()), own);

    // A cut day, a quiet one and one from before counting began measure
    // nothing, so the benchmark's ratio stands in.
    let mut input = server();
    input
        .raw
        .retain(|r| r.day < day("2026-11-18") || r.day > day("2026-11-20"));
    input.raw.push(raw("2026-11-18", 10 * GB, true));
    input.raw.push(raw("2026-11-19", 10 * MB, false));
    input.counted_since = Some(time::midnight(day("2026-11-20")) + 1);
    let prior = Tiers {
        hot_days: Some(3),
        ratio_ppm: 200_000,
        own: false,
    };
    assert_eq!(Tiers::of(&input), prior);

    // Without a hot day to compare with, too.
    let mut input = server();
    input.closed.clear();
    assert_eq!(Tiers::of(&input), prior);
}

#[test]
fn days_past_the_hot_days_take_their_cold_share() {
    let tiers = Tiers {
        hot_days: Some(7),
        ratio_ppm: 100_000,
        own: false,
    };
    // Today and 7 closed days at 10 GB, 23 at 1 GB.
    assert_eq!(tiers.steady(10 * GB, 30), 103 * GB);
    assert_eq!(tiers.steady(10 * GB, 5), 60 * GB);
    assert_eq!(tiers.cold(10 * GB), GB);
    let off = Tiers {
        hot_days: None,
        ..tiers
    };
    assert_eq!(off.steady(10 * GB, 30), 310 * GB);
    assert_eq!(off.hot(30), 30);
    assert_eq!(tiers.hot(30), 7);
}
