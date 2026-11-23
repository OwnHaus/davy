use super::schedule::{Crontab, Schedule, Zone, span};
use jiff::Timestamp;
use std::time::Duration;

fn ts(s: &str) -> Timestamp {
    s.parse().unwrap()
}

fn next(cron: &str, zone: &str, after: &str) -> Option<String> {
    let spec = Crontab::parse(cron).unwrap();
    spec.next(&Zone::parse(zone).unwrap(), ts(after))
        .map(|t| t.to_string())
}

#[test]
fn crontab_fields_and_names() {
    let cases = [
        ("0 * * * *", "2026-11-23T16:11:47Z", "2026-11-23T17:00:00Z"),
        ("0 * * * *", "2026-11-23T17:00:00Z", "2026-11-23T18:00:00Z"),
        (
            "*/15 9-17 * * mon-fri",
            "2026-11-21T12:00:00Z",
            "2026-11-23T09:00:00Z",
        ),
        (
            "*/15 9-17 * * MON-FRI",
            "2026-11-23T09:00:00Z",
            "2026-11-23T09:15:00Z",
        ),
        (
            "*/15 9-17 * * mon-fri",
            "2026-11-23T17:46:00Z",
            "2026-11-24T09:00:00Z",
        ),
        (
            "5,10-12 0 * * *",
            "2026-11-23T00:10:30Z",
            "2026-11-23T00:11:00Z",
        ),
        (
            "30/10 * * * *",
            "2026-11-23T00:55:00Z",
            "2026-11-23T01:30:00Z",
        ),
        (
            "0 0 1 jan,Jul *",
            "2026-11-23T00:00:00Z",
            "2027-01-01T00:00:00Z",
        ),
        ("0 0 * * 7", "2026-11-23T00:00:00Z", "2026-11-29T00:00:00Z"),
        ("0 0 * * 0", "2026-11-23T00:00:00Z", "2026-11-29T00:00:00Z"),
        ("0 0 29 2 *", "2026-03-01T00:00:00Z", "2028-02-29T00:00:00Z"),
        (
            "59 23 31 12 *",
            "2026-12-31T23:59:00Z",
            "2027-12-31T23:59:00Z",
        ),
        // Both day fields restricted: the 13th or any Friday, whichever is first.
        (
            "0 12 13 * 5",
            "2026-11-23T00:00:00Z",
            "2026-11-27T12:00:00Z",
        ),
        (
            "0 12 24 * 5",
            "2026-11-23T00:00:00Z",
            "2026-11-24T12:00:00Z",
        ),
        // A day field that starts with `*` is unrestricted, so both must match.
        (
            "0 12 */2 * 5",
            "2026-11-23T00:00:00Z",
            "2026-11-27T12:00:00Z",
        ),
        (
            "0 12 */2 * 2",
            "2026-11-23T00:00:00Z",
            "2026-12-01T12:00:00Z",
        ),
    ];
    for (cron, after, want) in cases {
        assert_eq!(
            next(cron, "UTC", after).as_deref(),
            Some(want),
            "{cron} after {after}"
        );
    }
}

#[test]
fn shortcuts_stand_for_their_expressions() {
    let cases = [
        ("@yearly", "0 0 1 1 *"),
        ("@ANNUALLY", "0 0 1 1 *"),
        ("@monthly", "0 0 1 * *"),
        ("@weekly", "0 0 * * 0"),
        ("@daily", "0 0 * * *"),
        ("@midnight", "0 0 * * *"),
        ("@hourly", "0 * * * *"),
        ("  0   *  * * *  ", "0 * * * *"),
    ];
    for (text, want) in cases {
        assert_eq!(Crontab::parse(text).unwrap().as_str(), want, "{text}");
    }
}

#[test]
fn bad_crontabs_are_refused() {
    let long = format!("0 * * * *{}", " ".repeat(200));
    for bad in [
        "",
        "* * * *",
        "* * * * * *",
        "60 * * * *",
        "* 24 * * *",
        "* * 0 * *",
        "* * 32 * *",
        "* * * 13 *",
        "* * * * 8",
        "*/0 * * * *",
        "*/61 * * * *",
        "5-1 * * * *",
        "a * * * *",
        "1,,2 * * * *",
        "- * * * *",
        "*/ * * * *",
        "@reboot",
        "0 0 31 2 *",
        "0 0 30,31 2 *",
        "0 0 31 4,6,9,11 *",
        long.as_str(),
    ] {
        assert_eq!(Crontab::parse(bad), None, "{bad:?}");
    }
}

#[test]
fn crontab_in_a_zone_across_dst() {
    // Europe/Prague moves from CET to CEST at 02:00 on 2027-03-28 and back at
    // 03:00 on 2026-10-25.
    let cases = [
        ("0 9 * * *", "2026-11-23T00:00:00Z", "2026-11-23T08:00:00Z"),
        ("0 9 * * *", "2026-07-01T00:00:00Z", "2026-07-01T07:00:00Z"),
        // 02:30 does not exist on the spring day, so it runs at 03:30 CEST.
        ("30 2 * * *", "2027-03-27T12:00:00Z", "2027-03-28T01:30:00Z"),
        ("30 2 * * *", "2027-03-28T01:30:00Z", "2027-03-29T00:30:00Z"),
        ("0 3 * * *", "2027-03-27T12:00:00Z", "2027-03-28T01:00:00Z"),
        // 02:30 happens twice on the autumn day and runs at the first, CEST.
        ("30 2 * * *", "2026-10-24T12:00:00Z", "2026-10-25T00:30:00Z"),
        ("30 2 * * *", "2026-10-25T00:31:00Z", "2026-10-26T01:30:00Z"),
        ("0 * * * *", "2026-10-25T00:30:00Z", "2026-10-25T02:00:00Z"),
    ];
    for (cron, after, want) in cases {
        assert_eq!(
            next(cron, "Europe/Prague", after).as_deref(),
            Some(want),
            "{cron} after {after}"
        );
    }
}

#[test]
fn zones_come_from_the_bundled_database() {
    assert_eq!(
        Zone::parse("Europe/Prague").unwrap().name(),
        "Europe/Prague"
    );
    assert_eq!(Zone::parse("UTC").unwrap().name(), "UTC");
    assert_eq!(Zone::utc().name(), "UTC");
    for bad in [
        "",
        "Mars/Olympus_Mons",
        "../etc/passwd",
        "/etc/localtime",
        &"A".repeat(65),
    ] {
        assert_eq!(Zone::parse(bad), None, "{bad:?}");
    }
}

#[test]
fn intervals_from_the_sdk() {
    let utc = Zone::utc;
    let every = |s| Some(Schedule::Every(Duration::from_secs(s)));
    assert_eq!(Schedule::interval(1, "minute", utc()), every(60));
    assert_eq!(Schedule::interval(2, "hour", utc()), every(7200));
    assert_eq!(Schedule::interval(3, "day", utc()), every(259_200));
    assert_eq!(Schedule::interval(2, "week", utc()), every(1_209_600));
    assert_eq!(Schedule::interval(366, "day", utc()), every(31_622_400));
    assert_eq!(
        Schedule::interval(1, "month", utc()),
        Some(Schedule::Months { n: 1, zone: utc() })
    );
    assert_eq!(
        Schedule::interval(12, "month", utc()),
        Some(Schedule::Months { n: 12, zone: utc() })
    );
    assert_eq!(
        Schedule::interval(1, "year", utc()),
        Some(Schedule::Months { n: 12, zone: utc() })
    );
    for (value, unit) in [
        (0, "minute"),
        (367, "day"),
        (53, "week"),
        (0, "month"),
        (13, "month"),
        (2, "year"),
        (1, "second"),
        (1, "fortnight"),
        (u64::MAX, "minute"),
        (u64::MAX, "year"),
    ] {
        assert_eq!(
            Schedule::interval(value, unit, utc()),
            None,
            "{value} {unit}"
        );
    }
}

#[test]
fn due_adds_the_margin_after_the_next_expected_time() {
    let ms = |s: &str| ts(s).as_millisecond();
    let at = |s: &Schedule, at: &str, margin: u64| {
        let due = s.due(ms(at), Duration::from_mins(margin)).unwrap();
        Timestamp::from_millisecond(due).unwrap().to_string()
    };
    let hourly = Schedule::Every(Duration::from_hours(1));
    assert_eq!(
        at(&hourly, "2026-11-23T16:00:00Z", 5),
        "2026-11-23T17:05:00Z"
    );
    let prague = Zone::parse("Europe/Prague").unwrap();
    let monthly = Schedule::Months {
        n: 1,
        zone: prague.clone(),
    };
    // 13:00 on 31 January in Prague; February has no 31st, so its last day.
    assert_eq!(
        at(&monthly, "2026-01-31T12:00:00Z", 0),
        "2026-02-28T12:00:00Z"
    );
    // A calendar month keeps the local time across the DST change.
    assert_eq!(
        at(&monthly, "2027-03-10T08:00:00Z", 1),
        "2027-04-10T07:01:00Z"
    );
    let cron = Schedule::Cron {
        spec: Crontab::parse("0 * * * *").unwrap(),
        zone: Zone::utc(),
    };
    // A ping 30 s early counts for 17:00, so the next is due at 18:00 plus margin.
    assert_eq!(at(&cron, "2026-11-23T16:59:30Z", 2), "2026-11-23T18:02:00Z");
    assert_eq!(at(&cron, "2026-11-23T17:00:10Z", 2), "2026-11-23T18:02:00Z");
    assert_eq!(at(&cron, "2026-11-23T17:30:00Z", 2), "2026-11-23T18:02:00Z");
    let daily = Schedule::Cron {
        spec: Crontab::parse("30 2 * * *").unwrap(),
        zone: prague,
    };
    assert_eq!(
        at(&daily, "2027-03-27T01:30:00Z", 1),
        "2027-03-28T01:31:00Z"
    );
    assert_eq!(hourly.due(i64::MAX - 1000, Duration::ZERO), None);
}

#[test]
fn columns_hold_exactly_one_schedule() {
    let prague = || Zone::parse("Europe/Prague").unwrap();
    let cron = Schedule::Cron {
        spec: Crontab::parse("0 * * * *").unwrap(),
        zone: prague(),
    };
    let cases = [
        (
            Schedule::Every(Duration::from_hours(1)),
            (3600, None, None, None),
        ),
        (
            Schedule::Months {
                n: 3,
                zone: prague(),
            },
            (0, Some(3), None, Some("Europe/Prague")),
        ),
        (cron, (0, None, Some("0 * * * *"), Some("Europe/Prague"))),
    ];
    for (schedule, columns) in cases {
        assert_eq!(schedule.columns(), columns);
        let (every_s, months, cron, zone) = columns;
        assert_eq!(
            Schedule::from_columns(every_s, months, cron, zone),
            Some(schedule)
        );
    }
    assert_eq!(
        Schedule::from_columns(0, Some(1), Some("0 * * * *"), Some("UTC")),
        None
    );
    assert_eq!(Schedule::from_columns(3600, None, None, Some("UTC")), None);
    assert_eq!(Schedule::from_columns(0, Some(1), None, None), None);
    assert_eq!(
        Schedule::from_columns(0, None, Some("0 * * * *"), Some("Mars/Base")),
        None
    );
}

#[test]
fn schedules_print_for_people() {
    let prague = || Zone::parse("Europe/Prague").unwrap();
    let cases = [
        (Schedule::Every(Duration::from_hours(2)), "every 2h"),
        (Schedule::Every(Duration::from_mins(90)), "every 90m"),
        (
            Schedule::Months {
                n: 1,
                zone: prague(),
            },
            "every month, Europe/Prague",
        ),
        (
            Schedule::Months {
                n: 12,
                zone: Zone::utc(),
            },
            "every 12 months, UTC",
        ),
        (
            Schedule::Cron {
                spec: Crontab::parse("0 * * * *").unwrap(),
                zone: prague(),
            },
            "cron 0 * * * *, Europe/Prague",
        ),
    ];
    for (schedule, want) in cases {
        assert_eq!(schedule.to_string(), want);
    }
    assert_eq!(span(Duration::from_hours(168)), "1w");
    assert_eq!(span(Duration::from_hours(72)), "3d");
    assert_eq!(span(Duration::from_secs(45)), "45s");
    assert_eq!(span(Duration::ZERO), "0s");
}
