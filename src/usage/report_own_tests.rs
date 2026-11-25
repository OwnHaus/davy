//! Davy's own project in the report: `davy usage` counts its lines under
//! it, and the forecast leaves them out, see [`Theirs`].

use super::*;
use crate::{
    logs::store::log_days,
    self_report::own,
    store,
    usage::{Count, stored},
};

const GB: u64 = 1_000_000_000;

fn day(s: &str) -> Date {
    s.parse().unwrap()
}

fn at(s: &str) -> i64 {
    time::nanos(s.parse().unwrap()).unwrap()
}

fn raw_day(d: &str, projects: &[(&str, u64)]) -> forecast::RawDay {
    forecast::RawDay {
        day: day(d),
        projects: projects
            .iter()
            .map(|(slug, bytes)| ((*slug).to_owned(), *bytes, false))
            .collect(),
    }
}

/// shop keeps logs 7 days and received errors, and no log arrived. Davy
/// started counting at `start` and, with `own_lines`, its own project
/// logged 600 bytes on each of `days`. The disk holds a closed log day of
/// 40 KB on each of `closed`.
fn report(start: &str, own_lines: bool, days: &[&str], closed: &[&str], now: &str) -> Report {
    let mut c = Connection::open_in_memory().unwrap();
    store::init(&mut c).unwrap();
    store::create_project(&c, "shop").unwrap();
    c.execute(
        "UPDATE projects SET log_retention_days=7, created_at=?1 WHERE slug='shop'",
        [at("2026-11-01T00:00:00Z") / 1_000_000],
    )
    .unwrap();
    c.execute_batch(
        "INSERT INTO usage_daily(day, project_id, signal, records, bytes) VALUES
         ('2026-11-20', 1, 'errors', 1000, 3000000),
         ('2026-11-21', 1, 'errors', 1000, 3000000),
         ('2026-11-22', 1, 'errors', 1000, 3000000);
         INSERT INTO issues(id,project_id,grouping_version,fingerprint,title,level,status,first_seen,last_seen,event_count) VALUES
         (1,1,1,'a','A','error','unresolved',0,0,3000);",
    )
    .unwrap();
    let mut rows = Vec::new();
    if own_lines {
        let (me, _) = own::find_or_create(&mut c).unwrap();
        rows = days
            .iter()
            .map(|d| log_days::Row {
                day: day(d),
                project: me.id,
                count: Count {
                    records: 2,
                    bytes: 600,
                },
            })
            .collect();
    }
    let flush = log_days::Flush {
        start: Some(at(start) / 1_000_000),
        drops: Vec::new(),
        segments: vec![log_days::Segment {
            rows,
            marks: Vec::new(),
        }],
    };
    stored::write(&c, &flush).unwrap();
    let req = Request {
        project: None,
        since: Some("1d".into()),
        what_if: WhatIf::default(),
        defaults: retention::Defaults::default(),
    };
    let disk = Disk {
        closed: closed.iter().map(|d| (day(d), 40_000)).collect(),
        today: 40_000,
        sqlite: GB,
        total: 500 * GB,
        free: 100 * GB,
        ..Disk::default()
    };
    prepare(&c, &req, at(now)).unwrap().finish(Ok(disk))
}

/// The forecast of shop's errors alone, as of a server where Davy logged
/// nothing.
const WHY: &str = "Davy counted no raw log bytes in the last 30 days and the disk holds no closed log day since 2026-11-17, so logs add nothing and the forecast rests on errors alone; confidence is low while errors that began less than 7 whole days ago add over 5% of the errors' growth";

fn basis(r: &Report) -> (String, String, String, u32) {
    let f = r.forecast.as_ref().unwrap();
    let logs = f.logs.as_ref().unwrap();
    let v = r.json(true);
    (
        logs.basis.name().to_owned(),
        v["basis"]["confidence"].as_str().unwrap().to_owned(),
        v["basis"]["why"].as_str().unwrap().to_owned(),
        f.longest_days,
    )
}

/// The minutes after a first start: Davy's line that it made its project
/// is counted, and a server that stores only errors still forecasts from
/// them, as if Davy had logged nothing.
#[test]
fn davy_s_own_lines_of_a_first_start_leave_the_forecast_on_errors() {
    let start = "2026-11-24T12:00:00Z";
    let now = "2026-11-24T12:01:00Z";
    let r = report(start, true, &["2026-11-24"], &[], now);
    assert_eq!(
        basis(&r),
        (
            "errors_only".to_owned(),
            "low".to_owned(),
            WHY.to_owned(),
            7,
        )
    );
    assert_eq!(r.forecast, report(start, false, &[], &[], now).forecast);
    let counted: Vec<(&str, u64)> = r
        .projects
        .iter()
        .map(|p| (p.slug.as_str(), p.stored.bytes))
        .collect();
    assert_eq!(counted, [("davy", 600), ("shop", 0)]);
}

/// A day later the disk holds a closed log day of Davy's own lines alone.
/// It is no log day of the forecast, nor do the 30 days Davy's own project
/// keeps logs lengthen the bound.
#[test]
fn a_closed_day_of_davy_s_own_lines_alone_is_no_log_day() {
    let start = "2026-11-23T00:00:00Z";
    let now = "2026-11-24T12:00:00Z";
    let days = ["2026-11-23", "2026-11-24"];
    let r = report(start, true, &days, &["2026-11-23"], now);
    assert_eq!(
        basis(&r),
        (
            "errors_only".to_owned(),
            "low".to_owned(),
            WHY.to_owned(),
            7,
        )
    );
    assert_eq!(r.forecast, report(start, false, &[], &[], now).forecast);
}

/// Only the project marked own is left out: a person's project with the
/// slug davy keeps its raw bytes, days and retention, and a day nobody
/// counted stays, as it may hold anyone's lines.
#[test]
fn theirs_leave_out_the_own_project_and_the_days_of_its_lines_alone() {
    let raw = [
        raw_day("2026-11-21", &[("davy-self", 600)]),
        raw_day("2026-11-22", &[("davy", 5_000), ("davy-self", 600)]),
        raw_day("2026-11-23", &[("davy-self", 600), ("shop", 9_000)]),
    ];
    let closed: Vec<(Date, u64)> = ["2026-11-20", "2026-11-21", "2026-11-22", "2026-11-23"]
        .iter()
        .map(|d| (day(d), 40_000))
        .collect();
    let projects = [("davy", 7), ("davy-self", 30), ("shop", 14)];
    let projects = projects.iter().map(|(s, n)| ((*s).to_owned(), *n));
    assert_eq!(
        Theirs::new(Some("davy-self"), &raw, &closed, projects),
        Theirs {
            raw: vec![
                raw_day("2026-11-22", &[("davy", 5_000)]),
                raw_day("2026-11-23", &[("shop", 9_000)]),
            ],
            closed: vec![
                (day("2026-11-20"), 40_000),
                (day("2026-11-22"), 40_000),
                (day("2026-11-23"), 40_000),
            ],
            projects: vec![("davy".to_owned(), 7), ("shop".to_owned(), 14)],
        }
    );
}
