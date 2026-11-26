use super::*;
use crate::{
    store,
    usage::{Count, Row},
};
use jiff::civil::date;
use std::collections::HashMap;

/// 2026-11-26 00:00 UTC.
const MIDNIGHT: i64 = 1_795_651_200_000_000_000;
const HOUR: i64 = 3_600_000_000_000;
const NOON: i64 = MIDNIGHT + 12 * HOUR;

fn memory() -> Connection {
    let mut c = Connection::open_in_memory().unwrap();
    store::init(&mut c).unwrap();
    store::create_project(&c, "a").unwrap();
    store::create_project(&c, "b").unwrap();
    c
}

fn quota(errors: Option<u64>, logs: Option<u64>) -> Settings {
    Settings {
        errors,
        logs,
        ..Settings::default()
    }
}

fn gate(project: i64, s: Settings) -> Gate {
    let g = Gate::default();
    g.load(HashMap::from([(project, s)]), NOON);
    g
}

#[test]
fn errors_count_events_and_logs_count_bytes() {
    let g = gate(1, quota(Some(3), Some(1000)));
    g.used(1, Signal::Errors, Count::new(2, 5000), NOON);
    g.used(1, Signal::Logs, Count::new(500, 999), NOON);
    assert_eq!(g.limits(1, NOON).quota, [false, false]);
    g.used(1, Signal::Errors, Count::new(1, 1), NOON);
    g.used(1, Signal::Logs, Count::new(1, 1), NOON);
    let l = g.limits(1, NOON);
    assert_eq!(l.quota, [true, true]);
    assert!(l.blocks(Signal::Errors) && l.blocks(Signal::Logs));
    assert!(!l.blocks(Signal::Sessions));
    assert_eq!(l.midnight, 43_200);
}

#[test]
fn a_project_without_a_quota_is_never_refused() {
    let g = gate(1, quota(Some(1), None));
    g.used(2, Signal::Errors, Count::new(1_000_000, 1), NOON);
    g.used(1, Signal::Logs, Count::new(1, usize::MAX), NOON);
    assert_eq!(g.limits(2, NOON).quota, [false, false]);
    assert_eq!(g.limits(1, NOON).quota, [false, false]);
}

#[test]
fn the_counts_start_again_at_midnight_utc() {
    let g = gate(1, quota(Some(1), None));
    g.used(1, Signal::Errors, Count::new(1, 1), NOON);
    let last = MIDNIGHT + 24 * HOUR - 1;
    assert_eq!(g.limits(1, last).quota, [true, false]);
    assert_eq!(g.limits(1, last).midnight, 1);
    let next = MIDNIGHT + 24 * HOUR;
    assert_eq!(g.limits(1, next).quota, [false, false]);
    assert_eq!(g.limits(1, next).midnight, 86_400);
}

#[test]
fn a_restart_restores_todays_counts_and_skips_other_days() {
    let g = gate(1, quota(Some(10), Some(100)));
    let counted = HashMap::from([
        ((1, Signal::Errors), Count::new(10, 1)),
        ((1, Signal::Logs), Count::new(1, 99)),
        ((1, Signal::Sessions), Count::new(5, 5)),
    ]);
    g.restore(date(2026, 11, 25), &counted, NOON);
    assert_eq!(g.limits(1, NOON).quota, [false, false]);
    g.restore(date(2026, 11, 26), &counted, NOON);
    assert_eq!(g.limits(1, NOON).quota, [true, false]);
}

#[test]
fn start_reads_the_settings_and_todays_stored_counts() {
    let c = memory();
    settings::store(&c, 1, &quota(Some(2), None)).unwrap();
    let today = date(2026, 11, 26);
    let row = |day, records| Row {
        day,
        project: 1,
        signal: Signal::Errors,
        count: Count::new(records, 10),
    };
    crate::usage::write(&c, &[row(today, 2), row(date(2026, 11, 25), 50)]).unwrap();
    let g = Gate::default();
    start(&c, &g, NOON).unwrap();
    assert_eq!(g.limits(1, NOON).quota, [true, false]);
    assert_eq!(g.limits(2, NOON).quota, [false, false]);
}

#[test]
fn refusals_are_written_once_and_kept_when_the_writer_fails() {
    let c = memory();
    let g = Gate::default();
    g.refuse(1, QUOTA, "error", 1, NOON);
    g.refuse(1, QUOTA, "error", 1, NOON);
    g.refuse(1, QUOTA, "log_item", 0, NOON);
    g.refuse(2, QUOTA, "log_item", 7, NOON);
    g.refuse_request(2, QUOTA, NOON);
    let flush = g.take();
    assert_eq!((flush.refused.len(), flush.requests.len()), (2, 1));
    assert!(g.take().is_empty());
    g.put_back(flush);
    let flush = g.take();
    write(&c, &flush).unwrap();
    write(&c, &flush).unwrap();
    let rows: Vec<(i64, i64, String, String, i64)> = c
        .prepare("SELECT day, project_id, reason, category, quantity FROM client_reports ORDER BY project_id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        rows,
        [
            (1_795_651_200_000, 1, "davy_quota".into(), "error".into(), 4),
            (
                1_795_651_200_000,
                2,
                "davy_quota".into(),
                "log_item".into(),
                14
            ),
        ]
    );
    let requests: (i64, i64, String, i64) = c
        .query_row(
            "SELECT day, project_id, reason, quantity FROM refused_requests",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(requests, (1_795_651_200_000, 2, "davy_quota".into(), 2));
    prune(&c, 1_795_651_200_000 + 90 * 86_400_000).unwrap();
    let kept: i64 = c
        .query_row("SELECT count(*) FROM refused_requests", [], |r| r.get(0))
        .unwrap();
    assert_eq!(kept, 1);
    prune(&c, 1_795_651_200_000 + 91 * 86_400_000).unwrap();
    let kept: i64 = c
        .query_row("SELECT count(*) FROM refused_requests", [], |r| r.get(0))
        .unwrap();
    assert_eq!(kept, 0);
}

#[test]
fn own_refusals_are_counted_past_the_cap_on_an_sdks_pairs() {
    let c = memory();
    let junk: Vec<client_report::Discarded> = (0..100)
        .map(|i| client_report::Discarded {
            reason: format!("made_up_{i}"),
            category: "error".into(),
            quantity: 1,
        })
        .collect();
    client_report::add(&c, 1, 1_795_651_200_000, &junk).unwrap();
    client_report::add_own(&c, 1, 1_795_651_200_000, QUOTA, "error", 3).unwrap();
    client_report::add_own(&c, 99, 1_795_651_200_000, QUOTA, "error", 3).unwrap();
    let n: i64 = c
        .query_row(
            "SELECT quantity FROM client_reports WHERE reason='davy_quota'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 3);
}

#[test]
fn settings_are_stored_and_read_back() {
    let c = memory();
    assert_eq!(settings::of(&c, 1).unwrap(), Settings::default());
    assert!(settings::all(&c).unwrap().is_empty());
    let s = Settings {
        errors: Some(5000),
        logs: Some(2_000_000_000),
        warn: 90,
        spike: Spike::Rate(250),
        spike_logs: Spike::Rate(20_000),
    };
    settings::store(&c, 1, &s).unwrap();
    assert_eq!(settings::of(&c, 1).unwrap(), s);
    assert_eq!(settings::all(&c).unwrap(), HashMap::from([(1, s)]));
    let off = Settings {
        spike: Spike::Off,
        spike_logs: Spike::Off,
        ..Settings::default()
    };
    settings::store(&c, 2, &off).unwrap();
    assert_eq!(settings::of(&c, 2).unwrap(), off);
    settings::store(&c, 1, &Settings::default()).unwrap();
    assert_eq!(settings::all(&c).unwrap(), HashMap::from([(2, off)]));
    assert_eq!(
        (s.rate(0), s.rate(1), off.rate(0), off.rate(1)),
        (Some(250), Some(20_000), None, None)
    );
    let defaults = Settings::default();
    assert_eq!((defaults.rate(0), defaults.rate(1)), (Some(4000), None));
}

#[test]
fn the_cli_reads_spike_rates() {
    assert_eq!(cli::spike("250"), Ok(Spike::Rate(250)));
    assert_eq!(cli::spike("off"), Ok(Spike::Off));
    assert_eq!(cli::spike("default"), Ok(Spike::Default));
    for bad in ["0", "-5", "1.5", "fast", ""] {
        assert!(cli::spike(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn the_cli_reads_counts_and_sizes() {
    use cli::{Limit, count, size};
    assert_eq!(count("5000"), Ok(Limit(Some(5000))));
    assert_eq!(count("off"), Ok(Limit(None)));
    assert!(count("0").is_err());
    assert!(count("-1").is_err());
    assert!(count("1k").is_err());
    assert_eq!(size("2GB"), Ok(Limit(Some(2_000_000_000))));
    assert_eq!(size("1.5 gb"), Ok(Limit(Some(1_500_000_000))));
    assert_eq!(size("500MB"), Ok(Limit(Some(500_000_000))));
    assert_eq!(size("1KB"), Ok(Limit(Some(1000))));
    assert_eq!(size("1000B"), Ok(Limit(Some(1000))));
    assert_eq!(size("0.001KB"), Ok(Limit(Some(1))));
    assert_eq!(size("1000TB"), Ok(Limit(Some(1_000_000_000_000_000))));
    assert_eq!(size("off"), Ok(Limit(None)));
    for bad in [
        "",
        "GB",
        "0",
        "0.0001KB",
        "2GiB",
        "1.2.3GB",
        "99999999TB",
        "1000.1TB",
        "1000",
        "-1MB",
        "-1GB",
    ] {
        assert!(size(bad).is_err(), "{bad:?}");
    }
    assert_eq!(
        size("2"),
        Err(
            "expected a size over 0 with a unit, B, KB, MB, GB or TB in powers of 1000, \
             such as 2GB or 500MB, at most 1 PB, or off, got \"2\""
                .into()
        )
    );
}

#[test]
fn the_status_shows_the_quota_todays_use_and_refusals() {
    let c = memory();
    settings::store(&c, 1, &quota(None, Some(2_000_000_000))).unwrap();
    let today = date(2026, 11, 26);
    let logs = Row {
        day: today,
        project: 1,
        signal: Signal::Logs,
        count: Count::new(10, 1_200_000_000),
    };
    crate::usage::write(&c, &[logs]).unwrap();
    client_report::add_own(&c, 2, 1_795_651_200_000, QUOTA, "error", 4).unwrap();
    let busy = |project, quantity| Requests {
        day: today,
        project,
        reason: BUSY,
        quantity,
    };
    let requests = vec![busy(1, 3), busy(NO_PROJECT, 5)];
    write(
        &c,
        &Flush {
            requests,
            ..Flush::default()
        },
    )
    .unwrap();
    let all = status::all(&c, NOON).unwrap();
    assert_eq!(
        all[&1].json(NOON),
        serde_json::json!({
            "errors": null,
            "logs": {"quota": 2_000_000_000, "today": 1_200_000_000, "percent": 60, "refusing": false},
            "warn_percent": 80,
            "spike": {
                "errors": {"setting": "default", "per_second": 4000},
                "logs": {"setting": "off", "per_second": null},
            },
            "resets_at": "2026-11-27T00:00:00Z",
            "refused_today": {"davy_quota": 0, "davy_spike": 0},
            "refused_log_requests_today": {"davy_quota": 0, "davy_spike": 0},
            "busy_refused_requests_today": 3,
        })
    );
    assert_eq!(
        all[&1].text(NOON),
        "errors: no quota\nlogs: 1.2 GB of 2.0 GB today (60%)\nwarn at: 80%\n\
         spike errors: 4000 a second, the default\nspike logs: off\n\
         resets at: 2026-11-27T00:00:00Z\n\
         refused today past a quota: 0 items, 0 log requests\n\
         refused today past a spike rate: 0 items, 0 log requests\n\
         refused today while busy: 3 requests\n"
    );
    assert_eq!(
        (all[&NO_PROJECT].busy, all[&NO_PROJECT].requests),
        (5, [0, 0])
    );
    assert_eq!(all[&2].refused, [4, 0]);
    assert_eq!(all[&2].settings, Settings::default());
    assert_eq!(status::percent(1, 3), 33);
    assert_eq!(status::percent(5, 0), 500);
}

#[test]
fn the_cli_refuses_a_negative_size_with_its_own_message() {
    use clap::Parser as _;
    let parsed = cli::Args::try_parse_from(["quota", "a", "--data", "d", "--logs", "-1GB"]);
    let message = parsed.err().unwrap().to_string();
    assert!(
        message.contains("expected a size over 0 with a unit, B, KB, MB, GB or TB"),
        "{message}"
    );
    let parsed = cli::Args::try_parse_from(["quota", "a", "--data", "d", "--errors", "-5"]);
    let message = parsed.err().unwrap().to_string();
    assert!(
        message.contains("expected a whole number over 0 or off, got \"-5\""),
        "{message}"
    );
}

#[tokio::test]
async fn the_cli_sets_a_quota_and_prints_it_for_a_person_or_as_json() {
    use clap::Parser as _;
    crate::test_support::away_from_midnight().await;
    let data = crate::test_support::scratch("quota-cli");
    let mut c = store::open(&data).unwrap();
    store::init(&mut c).unwrap();
    store::create_project(&c, "a").unwrap();
    drop(c);
    let dir = data.to_str().unwrap();
    let run = |extra: &[&str]| {
        let args = [&["quota", "a", "--data", dir][..], extra].concat();
        let mut out = Vec::new();
        cli::run(&mut out, &cli::Args::try_parse_from(args).unwrap()).unwrap();
        String::from_utf8(out).unwrap()
    };
    let text = run(&["--errors", "150", "--warn", "50"]);
    let resets = time::rfc3339(time::next_midnight(time::now_ns()));
    assert_eq!(
        text,
        format!(
            "project: a\nerrors: 0 of 150 today (0%)\nlogs: no quota\nwarn at: 50%\n\
             spike errors: 4000 a second, the default\nspike logs: off\n\
             resets at: {resets}\nrefused today past a quota: 0 items, 0 log requests\n\
             refused today past a spike rate: 0 items, 0 log requests\n\
             refused today while busy: 0 requests\n\
             A running server applies a change within 5 s.\n"
        )
    );
    let json: serde_json::Value = serde_json::from_str(&run(&["--json"])).unwrap();
    assert_eq!(
        json["errors"],
        serde_json::json!({"quota": 150, "today": 0, "percent": 0, "refusing": false})
    );
    assert_eq!(json["logs"], serde_json::Value::Null);
    assert_eq!(json["busy_refused_requests_today"], 0);
    std::fs::remove_dir_all(&data).unwrap();
}

#[tokio::test]
async fn the_cli_json_is_the_object_davy_usage_json_shows_of_the_project() {
    use clap::Parser as _;
    crate::test_support::away_from_midnight().await;
    let data = crate::test_support::scratch("quota-same");
    let mut c = store::open(&data).unwrap();
    store::init(&mut c).unwrap();
    store::create_project(&c, "a").unwrap();
    let set = Settings {
        logs: Some(2_000_000_000),
        spike_logs: Spike::Rate(100),
        ..Settings::default()
    };
    settings::store(&c, 1, &set).unwrap();
    let now = time::now_ns();
    let row = Row {
        day: time::day_of(now),
        project: 1,
        signal: Signal::Logs,
        count: Count::new(10, 1_200_000_000),
    };
    crate::usage::write(&c, &[row]).unwrap();
    let day_ms = time::midnight(time::day_of(now)) / 1_000_000;
    client_report::add_own(&c, 1, day_ms, SPIKE, "log_item", 7).unwrap();
    let args = ["quota", "a", "--data", data.to_str().unwrap(), "--json"];
    let mut out = Vec::new();
    cli::run(&mut out, &cli::Args::try_parse_from(args).unwrap()).unwrap();
    let quota: serde_json::Value = serde_json::from_slice(&out).unwrap();
    let req = crate::usage::Request {
        project: Some("a".into()),
        since: None,
        what_if: crate::usage::forecast::WhatIf::default(),
        defaults: crate::retention::Defaults::default(),
    };
    let usage = crate::usage::prepare(&c, &req, time::now_ns())
        .unwrap()
        .finish(Err("not walked".into()))
        .json(false);
    assert_eq!(usage["projects"][0]["limits"], quota);
    assert_eq!(quota["logs"]["today"], 1_200_000_000);
    assert_eq!(quota["refused_today"]["davy_spike"], 7);
    std::fs::remove_dir_all(&data).unwrap();
}
