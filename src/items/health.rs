//! What `release_health` returns: per release and environment, how many
//! sessions and users there were and how many of them crashed.

use crate::event::Timestamp;
use anyhow::Result;
use rusqlite::{Connection, params};
use serde::Serialize;

/// Rows `release_health` returns, newest release first.
const MAX_ROWS: i64 = 50;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ReleaseHealth {
    pub project: String,
    pub release: String,
    pub environment: String,
    pub sessions: i64,
    /// Sessions with at least one error, the crashed ones too.
    pub errored: i64,
    pub crashed: i64,
    /// Sessions the SDK ended without a clean exit or a crash, such as an
    /// app the system killed.
    pub abnormal: i64,
    /// Percent of sessions that did not crash, to two decimals.
    pub crash_free_sessions: Option<f64>,
    /// Users the SDK named. 0 when it names none, as server SDKs do.
    pub users: i64,
    pub crashed_users: i64,
    pub crash_free_users: Option<f64>,
    /// The hours of the first and the last session counted, RFC 3339.
    pub first_hour: String,
    pub last_hour: String,
}

/// Health of each release with sessions since `since`, in `project` or in
/// every project. Users count once per release however many days they
/// came back.
pub fn releases(
    c: &Connection,
    project: Option<i64>,
    since: Timestamp,
) -> Result<Vec<ReleaseHealth>> {
    let day = since.div_euclid(86_400_000) * 86_400_000;
    let mut q = c.prepare_cached(
        "WITH s AS (
            SELECT project_id, release, environment, sum(started) AS started, sum(errored) AS errored,
                   sum(crashed) AS crashed, sum(abnormal) AS abnormal, min(hour) AS first, max(hour) AS last
            FROM release_sessions WHERE hour>=?2 AND (?1 IS NULL OR project_id=?1)
            GROUP BY project_id, release, environment
         ), u AS (
            SELECT project_id, release, environment, count(DISTINCT user) AS users,
                   count(DISTINCT CASE WHEN flags&4 THEN user END) AS crashed_users
            FROM release_users WHERE day>=?3 AND (?1 IS NULL OR project_id=?1)
            GROUP BY project_id, release, environment
         )
         SELECT p.slug, s.release, s.environment, s.started, s.errored, s.crashed, s.abnormal,
                coalesce(u.users, 0), coalesce(u.crashed_users, 0), s.first, s.last
         FROM s JOIN projects p ON p.id=s.project_id
         LEFT JOIN u USING (project_id, release, environment)
         ORDER BY s.last DESC, s.started DESC LIMIT ?4",
    )?;
    let rows = q.query_map(params![project, since, day, MAX_ROWS], |r| {
        let sessions: i64 = r.get(3)?;
        let crashed: i64 = r.get(5)?;
        let users: i64 = r.get(7)?;
        let crashed_users: i64 = r.get(8)?;
        Ok(ReleaseHealth {
            project: r.get(0)?,
            release: r.get(1)?,
            environment: r.get(2)?,
            sessions,
            errored: r.get(4)?,
            crashed,
            abnormal: r.get(6)?,
            crash_free_sessions: crash_free(crashed, sessions),
            users,
            crashed_users,
            crash_free_users: crash_free(crashed_users, users),
            first_hour: crate::time::rfc3339_ms(r.get(9)?),
            last_hour: crate::time::rfc3339_ms(r.get(10)?),
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[expect(
    clippy::cast_precision_loss,
    reason = "counts stay far below 2^52, where f64 is exact"
)]
fn crash_free(crashed: i64, total: i64) -> Option<f64> {
    (total > 0).then(|| {
        let percent = 100.0 * (total - crashed.min(total)) as f64 / total as f64;
        (percent * 100.0).floor() / 100.0
    })
}

#[cfg(test)]
#[path = "health_tests.rs"]
mod tests;
