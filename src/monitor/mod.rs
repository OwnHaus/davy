//! Uptime monitors: the state each monitor is in and how a check result moves it.

#[cfg(test)]
mod tests;

/// Why a check failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reason {
    Timeout,
    Connect(String),
    Status(u16),
    KeywordMissing,
    KeywordFound,
    NotPublic,
    /// A heartbeat got no ping within its period and grace.
    Late,
    /// A heartbeat got a ping to `/fail`.
    Failed,
}

impl Reason {
    /// A heartbeat's grace period is its confirmation, so its failures open an
    /// incident without a second check.
    fn confirmed(&self) -> bool {
        matches!(self, Self::Late | Self::Failed)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Up,
    Down(Reason),
}

/// `Down` means the monitor has an open incident. The incident itself is a
/// row, so its ID lives there and not here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Pending,
    Up,
    Failing,
    Down,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    Opened(Reason),
    Resolved,
}

impl State {
    /// One failed HTTP check only moves a monitor to `Failing`. A second in a
    /// row opens an incident, and one success closes it.
    #[must_use]
    pub fn next(self, outcome: Outcome) -> (Self, Option<Change>) {
        match (self, outcome) {
            (Self::Down, Outcome::Up) => (Self::Up, Some(Change::Resolved)),
            (_, Outcome::Up) => (Self::Up, None),
            (Self::Down, Outcome::Down(_)) => (Self::Down, None),
            (Self::Failing, Outcome::Down(reason)) => (Self::Down, Some(Change::Opened(reason))),
            (_, Outcome::Down(reason)) if reason.confirmed() => {
                (Self::Down, Some(Change::Opened(reason)))
            }
            (Self::Pending | Self::Up, Outcome::Down(_)) => (Self::Failing, None),
        }
    }
}
