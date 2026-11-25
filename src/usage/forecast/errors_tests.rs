use super::*;
use crate::usage::footprint::FIXTURE;

const HOUR_MS: i64 = 3_600_000;

fn day(s: &str) -> Date {
    s.parse().unwrap()
}

fn at(s: &str) -> i64 {
    time::nanos(time::parse(s).unwrap()).unwrap()
}

/// Samples of SQLite from 812 MB and 1,204,000 events at `first`, each
/// `(hours, bytes, events)` later by that many hours and that much more.
fn store(first: &str, steps: &[(i64, u64, u64)]) -> Vec<StoreDay> {
    let first = at(first) / 1_000_000;
    let mut s = vec![StoreDay {
        day: time::day_of(first * 1_000_000),
        at_ms: first,
        bytes: 812_345_678,
        events: 1_204_000,
    }];
    for (hours, bytes, events) in steps {
        let last = *s.last().unwrap();
        let at_ms = last.at_ms + hours * HOUR_MS;
        s.push(StoreDay {
            day: time::day_of(at_ms * 1_000_000),
            at_ms,
            bytes: last.bytes.wrapping_add(*bytes),
            events: last.events.wrapping_add(*events),
        });
    }
    s
}

/// A day a little over 24 hours on, as the hourly task samples it.
fn daily(bytes: u64, events: u64) -> (i64, u64, u64) {
    (24, bytes, events)
}

#[test]
fn bytes_per_event_comes_from_daily_sqlite_growth() {
    let today = day("2026-11-24");
    let week = store("2026-11-16T00:04:11Z", &[daily(60_000_000, 100_000); 7]);
    assert_eq!(
        per_event(&week, today),
        PerEvent {
            own: Some(600),
            high: Some(600),
            days: 7
        }
    );
    // Unequal days: the median is the estimate's, the second highest the
    // bound's, and one day with pages from a burst of sessions moves
    // neither.
    let uneven = store(
        "2026-11-15T00:31:02Z",
        &[
            daily(59_000_000, 100_000),
            daily(72_000_000, 120_000),
            daily(57_000_000, 95_000),
            daily(84_700_000, 140_000),
            daily(76_800_000, 120_000),
            daily(66_550_000, 110_000),
            daily(24_000_000, 20_000),
            daily(57_000_000, 100_000),
        ],
    );
    // Per event: 590, 600, 600, 605, 640, 605, 1,200 and 570; the newest 7
    // drop the 590.
    assert_eq!(
        per_event(&uneven, today),
        PerEvent {
            own: Some(605),
            high: Some(640),
            days: 7
        }
    );
}

#[test]
fn a_day_with_few_new_events_does_not_measure_bytes_per_event() {
    let today = day("2026-11-24");
    let quiet = store(
        "2026-11-20T00:02:40Z",
        &[
            daily(6_000_000, 9_999),
            daily(6_200_000, 10_000),
            // Retention deleted more events than arrived, or freed pages.
            daily(1_000_000, 0u64.wrapping_sub(5_000)),
            daily(0u64.wrapping_sub(4_096), 12_000),
        ],
    );
    assert_eq!(
        per_event(&quiet, today),
        PerEvent {
            own: Some(620),
            high: Some(620),
            days: 1
        }
    );
}

#[test]
fn a_server_down_over_48_hours_gives_no_day() {
    let today = day("2026-11-24");
    let down = store(
        "2026-11-17T01:00:00Z",
        &[(48, 12_000_000, 20_000), (49, 13_000_000, 20_000)],
    );
    assert_eq!(per_event(&down, today).days, 1);
    assert_eq!(per_event(&down, today).own, Some(600));
    // A sample taken twice the same hour is no day either.
    let twice = store("2026-11-17T01:00:00Z", &[(0, 6_000_000, 10_000)]);
    assert_eq!(per_event(&twice, today).days, 0);
}

#[test]
fn days_older_than_the_look_back_do_not_count() {
    let old = store("2026-10-24T00:05:00Z", &[daily(60_000_000, 100_000)]);
    assert_eq!(per_event(&old, day("2026-11-24")).days, 1);
    assert_eq!(per_event(&old, day("2026-11-25")).days, 0);
    assert_eq!(per_event(&[], day("2026-11-24")).days, 0);
}

fn project(slug: &str, created: &str, daily: &[(&str, u64, u64)]) -> ProjectErrors {
    ProjectErrors {
        slug: slug.to_owned(),
        created: at(created),
        first: time::day_of(at(created)),
        daily: daily
            .iter()
            .map(|(d, records, bytes)| {
                (
                    day(d),
                    Count {
                        records: *records,
                        bytes: *bytes,
                    },
                )
            })
            .collect(),
        stored: 0,
        days: 30,
    }
}

#[test]
fn events_per_day_is_the_mean_of_bursty_days() {
    // One burst of 70,000 events in 7 days, 3 KB each as sent. The day 8
    // days ago and today are outside the window.
    let p = project(
        "shop",
        "2026-09-02T10:15:00Z",
        &[
            ("2026-11-16", 400_000, 1_200_000_000),
            ("2026-11-19", 70_000, 210_000_000),
            ("2026-11-24", 5_000, 15_000_000),
        ],
    );
    let now = at("2026-11-24T16:42:09Z");
    assert_eq!(
        rate(&p, day("2026-11-24"), now),
        (
            10_000,
            Count {
                records: 70_000,
                bytes: 210_000_000
            }
        )
    );
}

#[test]
fn a_new_project_counts_from_when_it_was_made() {
    let today = day("2026-11-24");
    for (created, now, daily, per_day) in [
        // Made 3 days ago at 14:20: 8,500 events over 57 h 40 min.
        (
            "2026-11-21T14:20:00Z",
            "2026-11-24T08:00:00Z",
            &[
                ("2026-11-21", 1_000, 2_000_000),
                ("2026-11-22", 4_000, 8_000_000),
                ("2026-11-23", 3_500, 7_000_000),
                ("2026-11-24", 900, 1_800_000),
            ][..],
            3_538,
        ),
        // Made at 23:30 yesterday: under a day closed, so up to now, 6 h.
        (
            "2026-11-23T23:30:00Z",
            "2026-11-24T05:30:00Z",
            &[("2026-11-23", 40, 80_000), ("2026-11-24", 200, 400_000)][..],
            960,
        ),
        // Made at 22:00 yesterday: 2 hours closed are no day either, so
        // 300 events over 12 h, not 100 over 2.
        (
            "2026-11-23T22:00:00Z",
            "2026-11-24T10:00:00Z",
            &[("2026-11-23", 100, 200_000), ("2026-11-24", 200, 400_000)][..],
            600,
        ),
        // Made at midnight yesterday: a whole day closed, and only it
        // counts.
        (
            "2026-11-23T00:00:00Z",
            "2026-11-24T12:00:00Z",
            &[
                ("2026-11-23", 5_000, 10_000_000),
                ("2026-11-24", 3_000, 6_000_000),
            ][..],
            5_000,
        ),
        // Made 20 minutes ago: counted over an hour at least.
        (
            "2026-11-24T09:30:00Z",
            "2026-11-24T09:50:00Z",
            &[("2026-11-24", 10, 20_000)][..],
            240,
        ),
        // Made today and quiet since.
        ("2026-11-24T09:30:00Z", "2026-11-24T17:00:00Z", &[][..], 0),
    ] {
        let p = project("tools", created, daily);
        assert_eq!(rate(&p, today, at(now)).0, per_day, "made {created}");
    }
}

#[test]
fn an_old_project_counts_from_its_first_errors() {
    let today = day("2026-11-24");
    let now = at("2026-11-24T12:00:00Z");
    let old = |first: &str, daily: &[(&str, u64, u64)]| ProjectErrors {
        first: day(first),
        ..project("api", "2026-09-02T10:15:00Z", daily)
    };
    // Made in September, its first errors yesterday: 600,000 a day, not
    // spread over the 6 days before it sent any.
    let p = old("2026-11-23", &[("2026-11-23", 600_000, 1_800_000_000)]);
    assert_eq!(rate(&p, today, now).0, 600_000);
    // Its first errors today: 6,000 in the 12 hours since midnight.
    let p = old("2026-11-24", &[("2026-11-24", 6_000, 18_000_000)]);
    assert_eq!(rate(&p, today, now).0, 12_000);
    // Errors since before the week: the mean of the 7 closed days.
    let p = old("2026-10-01", &[("2026-11-23", 700_000, 2_100_000_000)]);
    assert_eq!(rate(&p, today, now).0, 100_000);
}

#[test]
fn the_footprint_bytes_per_event_round_up_with_the_payload() {
    for (records, bytes, per_event) in [
        // 3,000 B sent, half kept as JSON: 600 + 1,500.
        (70_000, 210_000_000, 2_100),
        (1, 1_001, 1_101),
        (0, 0, 600),
    ] {
        assert_eq!(
            prior_bytes(FIXTURE, Count { records, bytes }),
            per_event,
            "{records} events of {bytes} B"
        );
    }
}

#[test]
fn a_project_at_its_error_steady_state_adds_nothing() {
    // shop keeps 30 days of a million events a day, and holds them.
    assert_eq!(
        part(1_000_000, 600, 30, 30_000_000),
        Part {
            growth: 600_000_000,
            room: 0
        }
    );
    // tools keeps 36,500 days of 10 a day and holds 1,000.
    assert_eq!(
        part(10, 600, 36_500, 1_000),
        Part {
            growth: 6_000,
            room: 218_400_000
        }
    );
    assert_eq!(
        part(u64::MAX, u64::MAX, u32::MAX, 0),
        Part {
            growth: u64::MAX,
            room: u64::MAX
        }
    );
}

/// shop: 10,000 events a day of 3 KB, keeping 120,000 of 30 days' worth.
fn shop() -> ProjectErrors {
    ProjectErrors {
        stored: 120_000,
        ..project(
            "shop",
            "2026-09-02T10:15:00Z",
            &[("2026-11-19", 70_000, 210_000_000)],
        )
    }
}

#[test]
fn errors_without_measured_days_take_the_footprint() {
    let today = day("2026-11-24");
    let now = at("2026-11-24T16:42:09Z");
    let quiet = project("tools", "2026-11-01T08:00:00Z", &[]);
    let found = errors(&[shop(), quiet], &[], FIXTURE, today, now).unwrap();
    // 2,100 B an event, 10,000 a day, and 180,000 events to go.
    let part = Part {
        growth: 21_000_000,
        room: 378_000_000,
    };
    assert_eq!(
        found,
        Found {
            errors: Errors {
                projects: 1,
                events_per_day: 10_000,
                bytes_per_event: 2_100,
                own_days: 0,
                basis: ErrorBasis::Footprint,
                growth: 21_000_000,
                estimate_growth: 21_000_000,
                share_ppm: 0,
            },
            bound: vec![part],
            estimate: vec![part],
            lean: Lean::Benchmark(0),
        }
    );
    let none = errors(
        &[project("tools", "2026-11-01T08:00:00Z", &[])],
        &[],
        FIXTURE,
        today,
        now,
    );
    assert_eq!(none, None);
    assert_eq!(errors(&[], &[], FIXTURE, today, now), None);
}

#[test]
fn the_bound_takes_the_high_end_of_measured_and_footprint() {
    let today = day("2026-11-24");
    let now = at("2026-11-24T16:42:09Z");
    // Three measured days of 700, 800 and 900 B an event, below shop's
    // 2,100 from the footprint: the bound keeps 2,100, the estimate
    // weighs 3 days of 800 against 4 of 2,100.
    let three = store(
        "2026-11-20T00:03:00Z",
        &[
            daily(7_000_000, 10_000),
            daily(9_000_000, 10_000),
            daily(8_000_000, 10_000),
        ],
    );
    let found = errors(&[shop()], &three, FIXTURE, today, now).unwrap();
    assert_eq!(
        (
            found.errors.basis,
            found.errors.own_days,
            found.errors.bytes_per_event
        ),
        (ErrorBasis::Blended, 3, 1_542)
    );
    assert_eq!(found.bound[0].growth, 21_000_000);
    assert_eq!(found.estimate[0].growth, 15_420_000);
    // Small events of 200 B: 700 from the footprint, and 7 measured days
    // whose second highest, 750, is above it. The estimate takes the
    // median, 710.
    let small = ProjectErrors {
        daily: vec![(
            day("2026-11-20"),
            Count {
                records: 70_000,
                bytes: 14_000_000,
            },
        )],
        ..shop()
    };
    let week = store(
        "2026-11-16T00:04:11Z",
        &[
            daily(6_900_000, 10_000),
            daily(7_000_000, 10_000),
            daily(7_050_000, 10_000),
            daily(7_100_000, 10_000),
            daily(7_200_000, 10_000),
            daily(7_500_000, 10_000),
            daily(9_000_000, 10_000),
        ],
    );
    let found = errors(&[small], &week, FIXTURE, today, now).unwrap();
    assert_eq!(
        (
            found.errors.basis,
            found.errors.bytes_per_event,
            found.bound[0].growth,
            found.estimate[0].growth
        ),
        (ErrorBasis::Measured, 710, 7_500_000, 7_100_000)
    );
    assert_eq!(found.errors.growth, 7_500_000);
}

#[test]
fn measured_bytes_per_event_split_by_each_projects_payloads() {
    let today = day("2026-11-24");
    let now = at("2026-11-24T16:42:09Z");
    // 7 measured days, median 710 B an event, from two projects of
    // 10,000 events a day each: shop's 3 KB events at 2,100 B from the
    // footprint, small's 200 B ones at 700. Their mean is 1,400, so shop
    // takes 710 x 2,100 / 1,400 = 1,065 B and small 355. Together they
    // still average the 710 SQLite measured.
    let week = store(
        "2026-11-16T00:04:11Z",
        &[
            daily(6_900_000, 10_000),
            daily(7_000_000, 10_000),
            daily(7_050_000, 10_000),
            daily(7_100_000, 10_000),
            daily(7_200_000, 10_000),
            daily(7_500_000, 10_000),
            daily(9_000_000, 10_000),
        ],
    );
    let small = project(
        "small",
        "2026-09-02T10:15:00Z",
        &[("2026-11-20", 70_000, 14_000_000)],
    );
    let found = errors(&[shop(), small], &week, FIXTURE, today, now).unwrap();
    let growth = |parts: &[Part]| parts.iter().map(|p| p.growth).collect::<Vec<_>>();
    assert_eq!(growth(&found.estimate), [10_650_000, 3_550_000]);
    assert_eq!(found.errors.bytes_per_event, 710);
    // The bound keeps each project's footprint where it is above the
    // measured high end, 750 B.
    assert_eq!(growth(&found.bound), [21_000_000, 7_500_000]);
}
