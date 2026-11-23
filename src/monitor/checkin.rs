//! The `check_in` envelope item of the Sentry SDKs, parsed into what a
//! heartbeat monitor needs. Pure: no clock, no I/O.

use super::schedule::{Crontab, Schedule, Zone};
use serde::Deserialize;
use serde_json::Value;
use std::{borrow::Cow, time::Duration};

/// Sentry's limit for a check-in item.
pub const MAX_ITEM: usize = 100 * 1024;
const MAX_SLUG: usize = 50;
pub const DEFAULT_MARGIN: Duration = Duration::from_mins(1);
const MAX_MARGIN: Duration = Duration::from_hours(24);
pub const DEFAULT_MAX_RUNTIME: Duration = Duration::from_mins(30);
/// Sentry clamps `max_runtime` to 7 days too.
const MAX_RUNTIME: Duration = Duration::from_hours(168);
const MAX_DURATION: Duration = Duration::from_hours(7 * 24);

/// 1 to 50 characters of `A-Z a-z 0-9 _ -`, kept in lower case, so a slug
/// is safe in a URL, a log line and a terminal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MonitorSlug(String);

impl MonitorSlug {
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        let ok = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'-';
        (!s.is_empty() && s.len() <= MAX_SLUG && s.bytes().all(ok))
            .then(|| Self(s.to_ascii_lowercase()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The ID of one run, 32 lowercase hex characters and never all zeros.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckInId(String);

impl CheckInId {
    /// The SDKs send a UUID with or without dashes. All zeros means "the open
    /// run", as does a missing or malformed ID.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        let hex: String = s.chars().filter(|c| *c != '-').collect();
        let ok = hex.len() == 32 && hex.bytes().all(|b| b.is_ascii_hexdigit());
        (ok && hex.bytes().any(|b| b != b'0')).then(|| Self(hex.to_ascii_lowercase()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    InProgress,
    Ok,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MonitorConfig {
    pub schedule: Schedule,
    pub margin: Duration,
    pub max_runtime: Duration,
}

/// An invalid config still lets the check-in ping a monitor that exists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Config {
    Absent,
    Valid(MonitorConfig),
    Invalid,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckIn {
    pub slug: MonitorSlug,
    pub status: Status,
    pub id: Option<CheckInId>,
    pub duration: Option<Duration>,
    pub config: Config,
}

/// Why a check-in was refused, counted per project and day in
/// `client_reports` with the category `monitor`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dropped {
    TooLarge,
    Invalid,
    BadSlug,
    UnknownMonitor,
    BadConfig,
    MonitorCap,
    Rate,
}

impl Dropped {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TooLarge => "davy_too_large",
            Self::Invalid => "davy_invalid",
            Self::BadSlug => "davy_bad_slug",
            Self::UnknownMonitor => "davy_unknown_monitor",
            Self::BadConfig => "davy_bad_config",
            Self::MonitorCap => "davy_monitor_cap",
            Self::Rate => "davy_rate",
        }
    }
}

/// A check-in on its way to the writer, which counts a refused one.
pub type Item = Result<CheckIn, Dropped>;

/// Fields are borrowed where they can be. The config is a tree, so a bad one
/// becomes `Config::Invalid` and leaves the rest of the item readable. Its
/// size is held by `MAX_ITEM`.
#[derive(Deserialize)]
struct Wire<'a> {
    #[serde(default, borrow)]
    check_in_id: Option<Cow<'a, str>>,
    #[serde(borrow)]
    monitor_slug: Cow<'a, str>,
    #[serde(borrow)]
    status: Cow<'a, str>,
    #[serde(default)]
    duration: Option<f64>,
    #[serde(default)]
    monitor_config: Option<Value>,
}

#[derive(Deserialize)]
struct WireConfig<'a> {
    #[serde(borrow)]
    schedule: WireSchedule<'a>,
    #[serde(default)]
    checkin_margin: Option<f64>,
    #[serde(default)]
    max_runtime: Option<f64>,
    #[serde(default, borrow)]
    timezone: Option<Cow<'a, str>>,
}

#[derive(Deserialize)]
struct WireSchedule<'a> {
    #[serde(rename = "type", borrow)]
    kind: Cow<'a, str>,
    value: Value,
    #[serde(default, borrow)]
    unit: Option<Cow<'a, str>>,
}

/// Parses one check-in item.
pub fn parse(payload: &[u8]) -> Item {
    if payload.len() > MAX_ITEM {
        return Err(Dropped::TooLarge);
    }
    let wire: Wire = serde_json::from_slice(payload).map_err(|_not_a_check_in| Dropped::Invalid)?;
    let status = match wire.status.as_ref() {
        "in_progress" => Status::InProgress,
        "ok" => Status::Ok,
        "error" => Status::Error,
        _ => return Err(Dropped::Invalid),
    };
    let slug = MonitorSlug::parse(&wire.monitor_slug).ok_or(Dropped::BadSlug)?;
    let duration = wire
        .duration
        .filter(|d| (0.0..=MAX_DURATION.as_secs_f64()).contains(d))
        .map(Duration::from_secs_f64);
    let config = match wire.monitor_config {
        None => Config::Absent,
        Some(tree) => config(&tree).map_or(Config::Invalid, Config::Valid),
    };
    Ok(CheckIn {
        slug,
        status,
        id: wire.check_in_id.as_deref().and_then(CheckInId::parse),
        duration,
        config,
    })
}

fn config(tree: &Value) -> Option<MonitorConfig> {
    let wire = WireConfig::deserialize(tree).ok()?;
    let zone = match wire.timezone.as_deref() {
        None | Some("") => Zone::utc(),
        Some(name) => Zone::parse(name)?,
    };
    let schedule = match (wire.schedule.kind.as_ref(), wire.schedule.unit.as_deref()) {
        ("crontab", _) => Schedule::Cron {
            spec: Crontab::parse(wire.schedule.value.as_str()?)?,
            zone,
        },
        ("interval", Some(unit)) => Schedule::interval(wire.schedule.value.as_u64()?, unit, zone)?,
        _ => return None,
    };
    let minutes = |m: Option<f64>, default: Duration, (low, high): (Duration, Duration)| match m {
        Some(m) if m >= 0.0 => Duration::from_secs_f64((m * 60.0).min(high.as_secs_f64())).max(low),
        _ => default,
    };
    Some(MonitorConfig {
        schedule,
        margin: minutes(
            wire.checkin_margin,
            DEFAULT_MARGIN,
            (Duration::ZERO, MAX_MARGIN),
        ),
        max_runtime: minutes(
            wire.max_runtime,
            DEFAULT_MAX_RUNTIME,
            (Duration::from_mins(1), MAX_RUNTIME),
        ),
    })
}
