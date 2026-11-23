//! Changes to a stored monitor that keep its ID, checks, incidents and
//! heartbeat token. The rules are in `docs/design.md`, "Editing a monitor".

use super::{
    CheckUrl, Expect, NewMonitor, Target,
    checkin::MonitorSlug,
    store::{self, Added},
};
use crate::{
    alert::{self, Incident, Notice},
    event::Timestamp,
};
use anyhow::{Context as _, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use std::time::Duration;

/// What `davy monitor edit` changes. `None` keeps what is stored.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Edit {
    /// Moves the monitor, with its checks and incidents, to this project.
    pub project: Option<i64>,
    pub name: Option<String>,
    pub every: Option<Duration>,
    pub url: Option<CheckUrl>,
    pub expect: Option<Expect>,
    pub timeout: Option<Duration>,
    pub grace: Option<Duration>,
    /// `Some(None)` removes the slug.
    pub slug: Option<Option<MonitorSlug>>,
}

impl Edit {
    fn changes_http(&self) -> bool {
        self.url.is_some() || self.expect.is_some() || self.timeout.is_some()
    }

    fn changes_heartbeat(&self) -> bool {
        self.grace.is_some() || self.slug.is_some()
    }
}

/// A monitor as stored before the edit.
struct Stored {
    project: i64,
    /// Made by check-ins.
    sdk: bool,
    name: String,
    every: Duration,
    target: Target,
    token: Option<String>,
}

impl Stored {
    fn slug(&self) -> Option<&MonitorSlug> {
        match &self.target {
            Target::Heartbeat { slug, .. } => slug.as_ref(),
            Target::Http { .. } => None,
        }
    }
}

/// Applies `e` to monitor `id` in place and returns what [`store::add`]
/// returns for it. Call it in a transaction: a move can queue a notice.
pub fn edit(c: &Connection, id: i64, e: &Edit, now: Timestamp) -> Result<Added> {
    let Some(old) = read(c, id)? else {
        bail!("no monitor {id}");
    };
    if old.sdk {
        return rename(c, id, &old, e);
    }
    let m = merged(id, &old, e)?;
    let project = e.project.unwrap_or(old.project);
    if let Target::Heartbeat {
        slug: Some(slug), ..
    } = &m.target
        && (project != old.project || old.slug() != Some(slug))
    {
        store::slug_free(c, project, slug.as_str())?;
    }
    write(c, id, project, &m)?;
    if project != old.project {
        carry_incident(c, id, project, &m.name, now)?;
    }
    let target = match &m.target {
        Target::Http { url, .. } => url.as_str().to_owned(),
        Target::Heartbeat { .. } => {
            let token = old
                .token
                .with_context(|| format!("heartbeat {id} has no token"))?;
            format!("/api/heartbeat/{token}")
        }
    };
    Ok(Added { id, target })
}

/// The stored monitor with `e` applied, held to the limits of `add`.
fn merged(id: i64, old: &Stored, e: &Edit) -> Result<NewMonitor> {
    let target = match &old.target {
        Target::Http {
            url,
            expect,
            timeout,
        } => {
            if e.changes_heartbeat() {
                bail!(
                    "monitor {id} is an HTTP check, so it has no grace period or slug. \
                     A monitor keeps its kind: add a heartbeat and remove this one"
                );
            }
            Target::Http {
                url: e.url.clone().unwrap_or_else(|| url.clone()),
                expect: e.expect.clone().unwrap_or_else(|| expect.clone()),
                timeout: e.timeout.unwrap_or(*timeout),
            }
        }
        Target::Heartbeat { grace, slug } => {
            if e.changes_http() {
                bail!(
                    "monitor {id} is a heartbeat, so it has no URL, keyword or timeout. \
                     A monitor keeps its kind: add an HTTP check and remove this one"
                );
            }
            Target::Heartbeat {
                grace: e.grace.unwrap_or(*grace),
                slug: e.slug.clone().unwrap_or_else(|| slug.clone()),
            }
        }
    };
    let name = e.name.as_deref().unwrap_or(&old.name);
    NewMonitor::new(name, target, Some(e.every.unwrap_or(old.every))).map_err(anyhow::Error::msg)
}

/// A monitor made by check-ins gets its schedule from them and is found by
/// project and slug, so its name is all an edit may change.
fn rename(c: &Connection, id: i64, old: &Stored, e: &Edit) -> Result<Added> {
    if e.project.is_some() || e.every.is_some() || e.changes_http() || e.changes_heartbeat() {
        bail!(
            "monitor {id} was made by check-ins, which set its schedule and find it by project \
             and slug, so only its name can change"
        );
    }
    if let Some(name) = &e.name {
        let name = super::monitor_name(name).map_err(anyhow::Error::msg)?;
        c.execute("UPDATE monitors SET name=?2 WHERE id=?1", params![id, name])?;
    }
    let slug = old
        .slug()
        .with_context(|| format!("monitor {id} was made by check-ins but has no slug"))?;
    Ok(Added {
        id,
        target: format!("check-in {}", slug.as_str()),
    })
}

/// The columns `url`, `expect`, `keyword` and `timeout_s` of an HTTP check.
type HttpColumns = (Option<String>, Option<String>, Option<String>, Option<i64>);

fn read(c: &Connection, id: i64) -> Result<Option<Stored>> {
    let row = c
        .query_row(
            "SELECT project_id,source='sdk',name,every_s,heartbeat_token,grace_s,slug,kind='http',url,expect,keyword,timeout_s
             FROM monitors WHERE id=?1",
            [id],
            |r| {
                let slug: Option<String> = r.get(6)?;
                let stored = Stored {
                    project: r.get(0)?,
                    sdk: r.get(1)?,
                    name: r.get(2)?,
                    every: secs(r.get(3)?),
                    token: r.get(4)?,
                    target: Target::Heartbeat {
                        grace: secs(r.get(5)?),
                        slug: slug.as_deref().and_then(MonitorSlug::parse),
                    },
                };
                let http: HttpColumns = (r.get(8)?, r.get(9)?, r.get(10)?, r.get(11)?);
                Ok((stored, r.get::<_, bool>(7)?.then_some(http)))
            },
        )
        .optional()?;
    let Some((mut stored, http)) = row else {
        return Ok(None);
    };
    if let Some((url, expect, keyword, timeout)) = http {
        stored.target = Target::Http {
            url: CheckUrl::parse(url.as_deref().unwrap_or_default())
                .map_err(|e| anyhow::anyhow!("HTTP monitor {id}: {e}"))?,
            expect: store::stored_expect(id, expect.as_deref(), keyword)?,
            timeout: secs(timeout),
        };
    }
    Ok(Some(stored))
}

fn secs(s: Option<i64>) -> Duration {
    Duration::from_secs(s.and_then(|s| u64::try_from(s).ok()).unwrap_or(0))
}

/// Stores `m` for `project`. A heartbeat that expects a ping expects it
/// `every` plus `grace` after its last one, as [`store::ping`] sets it.
fn write(c: &Connection, id: i64, project: i64, m: &NewMonitor) -> Result<()> {
    let secs = |d: Duration| d.as_secs().cast_signed();
    let (url, expect, keyword, timeout, grace, slug) = match &m.target {
        Target::Http {
            url,
            expect,
            timeout,
        } => {
            let (expect, keyword) = store::expect_columns(expect);
            let timeout = Some(secs(*timeout));
            (
                Some(url.as_str()),
                Some(expect),
                keyword,
                timeout,
                None,
                None,
            )
        }
        Target::Heartbeat { grace, slug } => {
            let slug = slug.as_ref().map(MonitorSlug::as_str);
            (None, None, None, None, Some(secs(*grace)), slug)
        }
    };
    c.execute(
        "UPDATE monitors SET project_id=?2,name=?3,url=?4,expect=?5,keyword=?6,timeout_s=?7,every_s=?8,grace_s=?9,slug=?10,
         due_at=iif(due_at IS NULL,NULL,last_ping_at+(?8+coalesce(?9,0))*1000) WHERE id=?1",
        params![
            id,
            project,
            m.name,
            url,
            expect,
            keyword,
            timeout,
            secs(m.every),
            grace,
            slug
        ],
    )?;
    Ok(())
}

/// Tells the channels of the new project about the incident a moved monitor
/// brings along, so the `monitor.up` that closes it is not the first they
/// hear of it. Channels of all projects heard of it when it opened.
fn carry_incident(c: &Connection, id: i64, project: i64, name: &str, now: Timestamp) -> Result<()> {
    let open = c
        .query_row(
            "SELECT id,reason FROM incidents WHERE monitor_id=?1 AND resolved_at IS NULL",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((incident, reason)) = open {
        let notice = Notice::MonitorDown(Incident {
            id: incident,
            monitor: id,
            name: name.to_owned(),
            reason,
        });
        alert::store::notify_project(c, project, &notice, now)?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "edit_tests.rs"]
mod tests;
