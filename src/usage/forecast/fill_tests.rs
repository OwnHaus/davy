use super::*;

fn day(s: &str) -> Date {
    s.parse().unwrap()
}

/// Logs of 10 GB a day with 30 GB of room left and errors of 5 GB a day
/// with 100 GB left: together 15 GB a day for 3 days, then the errors
/// alone.
const TWO: [Part; 2] = [
    Part {
        growth: 5,
        room: 100,
    },
    Part {
        growth: 10,
        room: 30,
    },
];

#[test]
fn piecewise_days_left_with_two_knees() {
    for (used, limit, days) in [
        // Before the first knee: 30 at 15 a day.
        (0, 30, Some(2)),
        (0, 45, Some(3)),
        // Past it: 45 by day 3, then 5 a day for the other 15.
        (0, 60, Some(6)),
        (13, 73, Some(6)),
        (0, 61, Some(7)),
        // Both parts full: 130 is all they add.
        (0, 130, Some(20)),
        (0, 131, None),
        // Other files filled the disk past the limit.
        (90, 80, Some(0)),
    ] {
        assert_eq!(days_left(used, limit, &TWO), days, "{used} of {limit}");
    }
}

#[test]
fn parts_without_growth_or_room_add_nothing() {
    let idle = [
        Part {
            growth: 0,
            room: 1_000,
        },
        Part {
            growth: 1_000,
            room: 0,
        },
    ];
    assert_eq!(days_left(0, 10, &idle), None);
    // Past the limit with nothing growing: other files filled it.
    assert_eq!(days_left(90, 80, &idle), None);
    assert_eq!(days_left(90, 80, &[]), None);
    let one = [idle[0], idle[1], TWO[1]];
    assert_eq!(days_left(0, 30, &one), Some(3));
}

#[test]
fn days_left_does_not_overflow_at_the_extremes() {
    let huge = [Part {
        growth: u64::MAX,
        room: u64::MAX,
    }; 3];
    assert_eq!(days_left(0, u64::MAX, &huge), Some(1));
    let slow = [Part {
        growth: 1,
        room: u64::MAX,
    }];
    assert_eq!(days_left(0, u64::MAX, &slow), Some(u64::MAX));
}

#[test]
fn a_date_past_ten_years_is_reported_as_beyond() {
    let today = day("2026-11-24");
    assert_eq!(
        Fill::new(today, Some(3_650)),
        Fill {
            days_left: Some(3_650),
            full_on: Some(day("2036-11-21")),
            beyond_10_years: false,
        }
    );
    for days in [Some(3_651), Some(u64::MAX)] {
        assert_eq!(
            Fill::new(today, days),
            Fill {
                days_left: None,
                full_on: None,
                beyond_10_years: true,
            }
        );
    }
    assert_eq!(
        Fill::new(today, None),
        Fill {
            days_left: None,
            full_on: None,
            beyond_10_years: false,
        }
    );
    assert_eq!(Fill::new(today, Some(0)).full_on, Some(today));
}
