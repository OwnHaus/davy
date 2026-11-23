//! Uptime monitors: what a monitor watches, the state it is in and how a
//! check result moves it.

pub mod check;
#[cfg(test)]
mod check_tests;
pub mod store;
#[cfg(test)]
mod tests;

use std::time::Duration;
use url::Url;

const MAX_URL: usize = 2048;
const MAX_NAME_CHARS: usize = 100;
const MAX_KEYWORD: usize = 1000;
const HTTP_EVERY: (Duration, Duration) = (Duration::from_secs(30), Duration::from_hours(24));
const HEARTBEAT_EVERY: (Duration, Duration) = (Duration::from_mins(1), Duration::from_hours(744));
const MAX_TIMEOUT: Duration = Duration::from_mins(1);
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
pub const DEFAULT_GRACE: Duration = Duration::from_mins(5);
const MAX_GRACE: Duration = Duration::from_hours(24);

/// An `http` or `https` URL with a host and no user info, so a check never
/// sends a credential and the stored URL holds none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckUrl(Url);

impl CheckUrl {
    pub fn parse(s: &str) -> Result<Self, String> {
        if s.len() > MAX_URL {
            return Err(format!("the URL is longer than {MAX_URL} bytes"));
        }
        let url = Url::parse(s).map_err(|e| format!("the URL is not valid: {e}"))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err("the URL must be http or https".into());
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err("the URL must not hold a user or password".into());
        }
        Ok(Self(url))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

/// 32 lowercase hex characters, the credential of a heartbeat monitor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeartbeatToken(String);

impl HeartbeatToken {
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        (s.len() == 32 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
            .then(|| Self(s.to_owned()))
    }

    fn random() -> Self {
        Self(uuid::Uuid::new_v4().simple().to_string())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expect {
    Status,
    Keyword(String),
    NoKeyword(String),
}

/// What a monitor watches. A heartbeat's token is made when it is stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Http {
        url: CheckUrl,
        expect: Expect,
        timeout: Duration,
    },
    Heartbeat {
        grace: Duration,
    },
}

/// A monitor that passed its limits and can be stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewMonitor {
    pub name: String,
    pub target: Target,
    pub every: Duration,
}

impl NewMonitor {
    /// Checks the limits in `docs/design.md`. Without `every`, an HTTP check
    /// runs every minute and a heartbeat expects a ping every hour.
    pub fn new(name: &str, target: Target, every: Option<Duration>) -> Result<Self, String> {
        let name = name.trim().to_owned();
        if name.is_empty() || name.chars().count() > MAX_NAME_CHARS {
            return Err(format!("the name must be 1 to {MAX_NAME_CHARS} characters"));
        }
        let within = |d: Duration, (low, high): (Duration, Duration)| (low..=high).contains(&d);
        let every = match &target {
            Target::Http {
                expect, timeout, ..
            } => {
                let every = every.unwrap_or(Duration::from_mins(1));
                if !within(every, HTTP_EVERY) {
                    return Err("an HTTP check runs every 30s to 24h".into());
                }
                if *timeout < Duration::from_secs(1) || *timeout > MAX_TIMEOUT || *timeout >= every
                {
                    return Err(
                        "the timeout must be 1s to 60s and shorter than the interval".into(),
                    );
                }
                if let Expect::Keyword(k) | Expect::NoKeyword(k) = expect
                    && (k.is_empty() || k.len() > MAX_KEYWORD)
                {
                    return Err(format!("the keyword must be 1 to {MAX_KEYWORD} bytes"));
                }
                every
            }
            Target::Heartbeat { grace } => {
                let every = every.unwrap_or(Duration::from_hours(1));
                if !within(every, HEARTBEAT_EVERY) {
                    return Err("a heartbeat expects a ping every 1m to 31d".into());
                }
                if *grace > MAX_GRACE {
                    return Err("the grace period must be 0s to 24h".into());
                }
                every
            }
        };
        Ok(Self {
            name,
            target,
            every,
        })
    }
}

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

impl std::fmt::Display for Reason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout => f.write_str("timeout"),
            Self::Connect(e) => write!(f, "connection failed: {e}"),
            Self::Status(s) => write!(f, "HTTP {s}"),
            Self::KeywordMissing => f.write_str("keyword missing"),
            Self::KeywordFound => f.write_str("keyword found"),
            Self::NotPublic => f.write_str("address is not public"),
            Self::Late => f.write_str("no ping in time"),
            Self::Failed => f.write_str("failure ping"),
        }
    }
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
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
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Up => "up",
            Self::Failing => "failing",
            Self::Down => "down",
        }
    }

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
