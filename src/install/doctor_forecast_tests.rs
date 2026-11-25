use super::*;
use crate::usage::{
    footprint::FIXTURE,
    forecast::{Logs, Spread},
};
use jiff::civil::Date;

const HOUR: i64 = 3_600_000_000_000;

/// A forecast on `basis` that puts the disk full in `days`, with both
/// projects keeping logs 30 days.
fn full_in(basis: Basis, days: u64) -> Forecast {
    let today: Date = "2026-11-24".parse().unwrap();
    let n = i64::try_from(days).unwrap();
    let full_on = Some(today.checked_add(jiff::Span::new().days(n)).unwrap());
    Forecast {
        growth: GIB,
        longest_days: 30,
        shortest_days: 30,
        steady_state: 100 * GIB,
        headroom: 10 * GIB,
        days_left: Some(days),
        full_on,
        unseen: None,
        logs: Some(Logs {
            basis,
            growth: Spread::flat(GIB),
            own_days: 0,
            possible_days: 7,
            raw: None,
            raw_days: 0,
            counted_ns: (basis == Basis::FirstDay).then_some(3 * HOUR / 2),
            recent: true,
            ratio: None,
            own_ratio: None,
            prior: FIXTURE,
            why: String::new(),
        }),
        full_on_range: [full_on; 2],
    }
}

#[test]
fn a_first_day_forecast_only_warns_while_the_disk_has_days_left() {
    let data = Path::new("/var/lib/davy");
    let free = Some(40 * GIB);
    let first = "the day rests on 1.5 hours of logs and the footprint benchmark; run doctor again once a whole day has closed before you shorten a retention with davy project retention SLUG --data /var/lib/davy --logs N, or add disk";
    let measured = "shorten retention with davy project retention SLUG --data /var/lib/davy --logs N, or add disk";
    for (basis, days, status, detail, fix) in [
        (
            Basis::FirstDay,
            1,
            Status::Warn,
            "40 GiB free, forecast full on 2026-11-25, in 1 day, a first day forecast from 1.5 hours of logs, low confidence",
            first,
        ),
        (
            Basis::FirstDay,
            0,
            Status::Fail,
            "40 GiB free, forecast full on 2026-11-24, in 0 days",
            first,
        ),
        (
            Basis::Measured,
            1,
            Status::Fail,
            "40 GiB free, forecast full on 2026-11-25, in 1 day",
            measured,
        ),
        (
            Basis::Blended,
            1,
            Status::Fail,
            "40 GiB free, forecast full on 2026-11-25, in 1 day",
            measured,
        ),
    ] {
        assert_eq!(
            disk(data, free, Some(Ok(full_in(basis, days)))),
            Check {
                name: "disk",
                status,
                detail: detail.to_owned(),
                fix: Some(fix.to_owned()),
            },
            "{basis:?} {days}"
        );
    }
}
