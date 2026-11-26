//! The whole answer of a server with logs and errors, pinned byte for
//! byte, so a change for servers without logs cannot move it, and the
//! answer of a server whose forecast rests on errors alone.

use super::*;
use crate::usage::{
    Count,
    footprint::FIXTURE,
    forecast::{Input, ProjectErrors, RawDay},
};
use jiff::ToSpan as _;

const GB: u64 = 1_000_000_000;
const MB: u64 = 1_000_000;

fn day(s: &str) -> Date {
    s.parse().unwrap()
}

/// 7 measured days of 1.4 GB raw, api keeping logs 30 days and shop 7,
/// shop's errors on the footprint benchmark, and the disk 5 GB short of
/// its limit.
fn logs_server() -> Input {
    let first = day("2026-11-17");
    let sizes = [900, 850, 950, 900, 1_000, 800, 900];
    let days: Vec<Date> = (0..7)
        .map(|i| first.checked_add(i.days()).unwrap())
        .collect();
    Input {
        today: day("2026-11-24"),
        closed: days.iter().zip(sizes).map(|(d, s)| (*d, s * MB)).collect(),
        davy: 9 * GB,
        fixed: 2 * GB,
        used: 445 * GB,
        total: 500 * GB,
        projects: vec![("api".to_owned(), 30), ("shop".to_owned(), 7)],
        default_days: 30,
        raw: days
            .iter()
            .map(|d| RawDay {
                day: *d,
                projects: vec![
                    ("api".to_owned(), GB, false),
                    ("shop".to_owned(), 400 * MB, false),
                ],
            })
            .collect(),
        counted_since: Some(time::midnight(day("2026-10-01"))),
        now: time::midnight(day("2026-11-24")) + 12 * time::HOUR_NS,
        store: Vec::new(),
        errors: vec![ProjectErrors {
            slug: "shop".to_owned(),
            created: time::midnight(day("2026-09-02")),
            first: day("2026-09-02"),
            daily: vec![(
                day("2026-11-19"),
                Count {
                    records: 700_000,
                    bytes: 2_100_000_000,
                },
            )],
            stored: 1_200_000,
            days: 30,
        }],
        prior: FIXTURE,
    }
}

fn report_of(input: &Input, today: u64) -> Report {
    Report {
        since: day("2026-11-17"),
        days: 7.5,
        disk: Ok(Disk {
            closed: input.closed.clone(),
            today,
            sqlite: 2 * GB,
            total: input.total,
            free: input.total - input.used,
            ..Disk::default()
        }),
        forecast: forecast::forecast(input, &WhatIf::default()),
        what_if: WhatIf::default(),
        projects: Vec::new(),
        default_log_days: None,
        now: 0,
        busy_no_project: 0,
    }
}

#[test]
fn a_server_with_logs_answers_byte_for_byte_as_before() {
    let r = report_of(&logs_server(), 400 * MB);
    let mut out = Vec::new();
    r.text(&mut out).unwrap();
    assert_eq!(String::from_utf8(out).unwrap(), TEXT);
    assert_eq!(r.json(false).to_string(), JSON);
}

/// No logs and shop's errors on the footprint benchmark: 100 000 events a
/// day of 3 000 bytes as sent take 600 + 1 500 = 2 100 bytes each in
/// SQLite, 210 MB a day. 1 GB to the limit is 4.8 days, so 5.
fn errors_server() -> Input {
    Input {
        closed: Vec::new(),
        davy: 3 * GB,
        fixed: 3 * GB,
        used: 449 * GB,
        projects: vec![("shop".to_owned(), 7)],
        raw: Vec::new(),
        ..logs_server()
    }
}

#[test]
fn a_server_with_errors_alone_answers_from_them() {
    let r = report_of(&errors_server(), 0);
    let mut out = Vec::new();
    r.text(&mut out).unwrap();
    assert_eq!(
        (String::from_utf8(out).unwrap(), r.json(false).to_string()),
        (ERRORS_TEXT.to_owned(), ERRORS_JSON.to_owned())
    );
}

const ERRORS_JSON: &str = concat!(
    r#"{"basis":{"confidence":"medium","errors":{"basis":"footprint","#,
    r#""bytes_per_event":2100,"estimate_growth_per_day":210000000,"events_per_day":100000,"#,
    r#""growth_per_day":210000000,"own_days":0,"projects":1},"full_on_range":["2026-11-29","#,
    r#""2026-11-29"],"logs":{"basis":"errors_only","counted_hours":null,"#,
    r#""footprint":"fixture","footprint_ratio":0.761,"growth_per_day":{"high":0,"#,
    r#""low":0,"mid":0},"own_days":0,"own_ratio":null,"possible_days":7,"#,
    r#""ratio":null,"ratio_range":null,"raw_days":7,"raw_per_day":0},"#,
    r#""why":"Davy counted no raw log bytes in the last 30 days and the disk holds no closed log day since 2026-11-17,"#,
    r#" so logs add nothing and the forecast rests on errors alone; confidence is medium as SQLite growth measured bytes per event on 0 of 7 days and the footprint benchmark gives the rest"},"#,
    r#""busy_refused_requests_today_no_project":0,"#,
    r#""davy":{"logs":0,"logs_today":0,"other":0,"sqlite":2000000000,"#,
    r#""uploads":0},"disk":{"beyond_10_years":false,"days_left":5,"free":51000000000,"#,
    r#""full_on":"2026-11-29","headroom":50000000000,"headroom_reason":"10% of the filesystem stays free: filesystems reserve blocks,"#,
    r#" and index merges and SQLite checkpoints need room for a copy of what they rewrite","#,
    r#""partial":null,"past_limit":false,"room_left":1000000000,"#,
    r#""steady_state":6780000000,"total":500000000000,"#,
    r#""used":449000000000,"worst_case":false},"estimate":{"beyond_10_years":false,"#,
    r#""clamped":false,"days_left":5,"full_on":"2026-11-29","steady_state":6780000000},"#,
    r#""forecast":"full on 2026-11-29, in 5 days, at 210.0 MB of errors a day","#,
    r#""growth_per_day":0,"longest_log_days":7,"projects":[],"shortest_log_days":7,"#,
    r#""window":{"days":7.5,"since":"2026-11-17"}}"#,
);

const ERRORS_TEXT: &str = concat!(
    "window    since 2026-11-17 (7.5 days)\n",
    "disk      449.0 GB used of 500.0 GB, 51.0 GB free\n",
    "davy      logs 0 B (today 0 B), sqlite 2.0 GB, uploads 0 B, other 0 B\n",
    "growth    210.0 MB of errors a day, steady state 6.8 GB\n",
    "forecast  full on 2026-11-29, in 5 days, at 210.0 MB of errors a day\n",
    "estimate  full on 2026-11-29, in 5 days, steady state 6.8 GB, at the middle bytes per event of errors\n",
    "basis     errors_only, medium confidence: Davy counted no raw log bytes in the last 30 days and the disk holds no closed log day since 2026-11-17, so logs add nothing and the forecast rests on errors alone; confidence is medium as SQLite growth measured bytes per event on 0 of 7 days and the footprint benchmark gives the rest\n",
    "errors    100000 events a day from 1 project at 2100 B each in SQLite, footprint: bytes per event measured on 0 of 7 days of SQLite growth here; the bound takes 2100 B each, 210.0 MB a day\n",
    "\n",
    "project              signal         records      bytes  records/day  bytes/day\n",
);

/// What the code answered before servers without logs had a forecast.
const JSON: &str = concat!(
    r#"{"basis":{"confidence":"medium","errors":{"basis":"footprint","#,
    r#""bytes_per_event":2100,"estimate_growth_per_day":210000000,"#,
    r#""events_per_day":100000,"growth_per_day":210000000,"own_days":0,"#,
    r#""projects":1},"full_on_range":["2026-11-29","2026-11-29"],"#,
    r#""logs":{"basis":"measured","counted_hours":null,"footprint":"fixture","#,
    r#""footprint_ratio":0.761,"growth_per_day":{"high":950000000,"#,
    r#""low":850000000,"mid":900000000},"own_days":7,"own_ratio":0.6429,"#,
    r#""possible_days":7,"ratio":0.6429,"ratio_range":[0.6071,0.6786],"#,
    r#""raw_days":7,"raw_per_day":1400000000},"#,
    r#""why":"7 closed log days measured on this server: the growth is their median size on disk; confidence is medium as errors add over 5% of what the bound adds until the disk fills,"#,
    r#" or of the room left to the limit within 10 years where it never fills,"#,
    r#" and SQLite growth measured their bytes per event on 0 of 7 days"},"#,
    r#""busy_refused_requests_today_no_project":0,"#,
    r#""davy":{"logs":6700000000,"logs_today":400000000,"other":0,"#,
    r#""sqlite":2000000000,"uploads":0},"disk":{"beyond_10_years":false,"#,
    r#""days_left":5,"free":55000000000,"full_on":"2026-11-29","#,
    r#""headroom":50000000000,"#,
    r#""headroom_reason":"10% of the filesystem stays free: filesystems reserve blocks,"#,
    r#" and index merges and SQLite checkpoints need room for a copy of what they rewrite","#,
    r#""partial":null,"past_limit":false,"room_left":5000000000,"#,
    r#""steady_state":33680000000,"total":500000000000,"#,
    r#""used":445000000000,"worst_case":true},"#,
    r#""estimate":{"beyond_10_years":false,"clamped":false,"days_left":6,"#,
    r#""full_on":"2026-11-30","steady_state":27765714258},"#,
    r#""forecast":"full on 2026-11-29, in 5 days at the earliest,"#,
    r#" at 900.0 MB of logs and 210.0 MB of errors a day. This is a worst case: projects keep logs 7 to 30 days and share one index per day,"#,
    r#" so Davy cannot tell each project's share and takes all logs as kept 30 days. At the current rate of logs,"#,
    r#" the disk fills on this day or later,"#,
    r#" or never; the estimate keeps each project's logs as long as it keeps them. Watch free space over a few days before you shorten a retention for this date","#,
    r#""growth_per_day":900000000,"longest_log_days":30,"projects":[],"#,
    r#""shortest_log_days":7,"window":{"days":7.5,"since":"2026-11-17"}}"#,
);

const TEXT: &str = concat!(
    "window    since 2026-11-17 (7.5 days)\n",
    "disk      445.0 GB used of 500.0 GB, 55.0 GB free\n",
    "davy      logs 6.7 GB (today 400.0 MB), sqlite 2.0 GB, uploads 0 B, other 0 B\n",
    "growth    900.0 MB of logs and 210.0 MB of errors a day, steady state 33.7 GB\n",
    "forecast  full on 2026-11-29, in 5 days at the earliest, at 900.0 MB of logs and 210.0 MB of errors a day. This is a worst case: projects keep logs 7 to 30 days and share one index per day, so Davy cannot tell each project's share and takes all logs as kept 30 days. At the current rate of logs, the disk fills on this day or later, or never; the estimate keeps each project's logs as long as it keeps them. Watch free space over a few days before you shorten a retention for this date\n",
    "estimate  full on 2026-11-30, in 6 days, steady state 27.8 GB, with each project's logs kept as long as it keeps them and packing alike\n",
    "basis     measured, medium confidence: 7 closed log days measured on this server: the growth is their median size on disk; confidence is medium as errors add over 5% of what the bound adds until the disk fills, or of the room left to the limit within 10 years where it never fills, and SQLite growth measured their bytes per event on 0 of 7 days\n",
    "errors    100000 events a day from 1 project at 2100 B each in SQLite, footprint: bytes per event measured on 0 of 7 days of SQLite growth here; the bound takes 2100 B each, 210.0 MB a day\n",
    "\n",
    "project              signal         records      bytes  records/day  bytes/day\n",
);

/// [`errors_server`] 10 GB short of its limit: shop's error retention
/// holds 1 800 000 more events of 2 100 bytes, 3.78 GB, so its errors
/// never fill the disk and Davy stays at 3 + 3.78 = 6.78 GB.
#[test]
fn errors_that_never_fill_the_disk_say_so() {
    let input = Input {
        used: 440 * GB,
        ..errors_server()
    };
    let r = report_of(&input, 0);
    let mut out = Vec::new();
    r.text(&mut out).unwrap();
    assert_eq!(
        (String::from_utf8(out).unwrap(), r.json(false).to_string()),
        (NEVER_TEXT.to_owned(), NEVER_JSON.to_owned())
    );
}

const NEVER_JSON: &str = concat!(
    r#"{"basis":{"confidence":"medium","errors":{"basis":"footprint","#,
    r#""bytes_per_event":2100,"estimate_growth_per_day":210000000,"#,
    r#""events_per_day":100000,"growth_per_day":210000000,"own_days":0,"projects":1},"#,
    r#""full_on_range":[null,null],"logs":{"basis":"errors_only","#,
    r#""counted_hours":null,"footprint":"fixture","footprint_ratio":0.761,"#,
    r#""growth_per_day":{"high":0,"low":0,"mid":0},"own_days":0,"own_ratio":null,"#,
    r#""possible_days":7,"ratio":null,"ratio_range":null,"raw_days":7,"#,
    r#""raw_per_day":0},"#,
    r#""why":"Davy counted no raw log bytes in the last 30 days and the disk holds no closed log day since 2026-11-17,"#,
    r#" so logs add nothing and the forecast rests on errors alone; confidence is medium as SQLite growth measured bytes per event on 0 of 7 days and the footprint benchmark gives the rest"},"#,
    r#""busy_refused_requests_today_no_project":0,"#,
    r#""davy":{"logs":0,"logs_today":0,"other":0,"sqlite":2000000000,"uploads":0},"#,
    r#""disk":{"beyond_10_years":false,"days_left":null,"free":60000000000,"#,
    r#""full_on":null,"headroom":50000000000,"#,
    r#""headroom_reason":"10% of the filesystem stays free: filesystems reserve blocks,"#,
    r#" and index merges and SQLite checkpoints need room for a copy of what they rewrite","#,
    r#""partial":null,"past_limit":false,"room_left":10000000000,"#,
    r#""steady_state":6780000000,"total":500000000000,"#,
    r#""used":440000000000,"worst_case":false},"estimate":{"beyond_10_years":false,"#,
    r#""clamped":false,"days_left":null,"full_on":null,"steady_state":6780000000},"#,
    r#""forecast":"errors never fill the disk at the current rate: each project's error retention holds Davy at 6.8 GB","#,
    r#""growth_per_day":0,"longest_log_days":7,"projects":[],"shortest_log_days":7,"#,
    r#""window":{"days":7.5,"since":"2026-11-17"}}"#,
);

const NEVER_TEXT: &str = concat!(
    "window    since 2026-11-17 (7.5 days)\n",
    "disk      440.0 GB used of 500.0 GB, 60.0 GB free\n",
    "davy      logs 0 B (today 0 B), sqlite 2.0 GB, uploads 0 B, other 0 B\n",
    "growth    210.0 MB of errors a day, steady state 6.8 GB\n",
    "forecast  errors never fill the disk at the current rate: each project's error retention holds Davy at 6.8 GB\n",
    "estimate  never full at the current rate, steady state 6.8 GB, at the middle bytes per event of errors\n",
    "basis     errors_only, medium confidence: Davy counted no raw log bytes in the last 30 days and the disk holds no closed log day since 2026-11-17, so logs add nothing and the forecast rests on errors alone; confidence is medium as SQLite growth measured bytes per event on 0 of 7 days and the footprint benchmark gives the rest\n",
    "errors    100000 events a day from 1 project at 2100 B each in SQLite, footprint: bytes per event measured on 0 of 7 days of SQLite growth here; the bound takes 2100 B each, 210.0 MB a day\n",
    "\n",
    "project              signal         records      bytes  records/day  bytes/day\n",
);

/// Shop's first 10 log records today have no log basis: the summary says
/// the disk may fill earlier once logs count.
#[test]
fn logs_left_out_of_the_forecast_say_the_disk_may_fill_earlier() {
    let mut input = errors_server();
    input.raw = vec![RawDay {
        day: input.today,
        projects: vec![("shop".to_owned(), 1_869, false)],
    }];
    assert_eq!(
        report_of(&input, 0).summary(),
        "full on 2026-11-29, in 5 days, at 210.0 MB of errors a day. Logs are not in this forecast yet, so the disk may fill earlier once they count"
    );
}
