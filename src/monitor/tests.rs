use super::*;

#[test]
fn a_monitor_moves_through_its_states_on_each_outcome() {
    use {Change::*, Outcome::Up as Ok, State::*};
    let down = |r: Reason| Outcome::Down(r);
    let http = Reason::Status(503);
    let cases = [
        (Pending, Ok, Up, None),
        (Pending, down(http.clone()), Failing, None),
        (
            Pending,
            down(Reason::Late),
            Down,
            Some(Opened(Reason::Late)),
        ),
        (
            Pending,
            down(Reason::Failed),
            Down,
            Some(Opened(Reason::Failed)),
        ),
        (Up, Ok, Up, None),
        (Up, down(http.clone()), Failing, None),
        (Up, down(Reason::Late), Down, Some(Opened(Reason::Late))),
        (Up, down(Reason::Failed), Down, Some(Opened(Reason::Failed))),
        (Failing, Ok, Up, None),
        (
            Failing,
            down(http.clone()),
            Down,
            Some(Opened(Reason::Status(503))),
        ),
        (
            Failing,
            down(Reason::Late),
            Down,
            Some(Opened(Reason::Late)),
        ),
        (
            Failing,
            down(Reason::Failed),
            Down,
            Some(Opened(Reason::Failed)),
        ),
        (Down, Ok, Up, Some(Resolved)),
        (Down, down(http), Down, None),
        (Down, down(Reason::Late), Down, None),
        (Down, down(Reason::Failed), Down, None),
    ];
    for (from, outcome, to, change) in cases {
        let label = format!("{from:?} + {outcome:?}");
        assert_eq!(from.next(outcome), (to, change), "{label}");
    }
}
