//! When a search last asked for a pattern (`docs/design.md`, "Last
//! queried"). Approximate: a search is matched on its filters against the
//! pattern's one example, not against the lines it returned.

use crate::logs::store::Examples;
use anyhow::Result;
use rusqlite::Connection;

/// The newest searches read, at most, by a walk down the primary key.
pub const HISTORY: i64 = 1_000;

/// A pattern as a search is matched against it.
pub struct Target<'a> {
    pub id: u64,
    /// The highest level of its records, as [`crate::logs::LogLevel`]
    /// numbers it.
    pub highest: Option<u8>,
    pub example: &'a str,
}

/// For each of `targets`, the time in milliseconds of the newest of the last
/// [`HISTORY`] searches that asked for it. A search asks for a pattern
/// when its project is `project`, every project, or `project` is `None`,
/// its minimum level is at most the pattern's highest, its pattern filter
/// is the pattern and its text finds the example. A search with neither
/// text nor a pattern filter browses and asks for none. The window is not
/// compared: the example has no time.
pub fn last_queried(
    c: &Connection,
    project: Option<i64>,
    targets: &[Target<'_>],
) -> Result<Vec<Option<i64>>> {
    let mut out = vec![None; targets.len()];
    if targets.is_empty() {
        return Ok(out);
    }
    // The index of the examples costs RAM and time: built for the first
    // search with text, not for a history of pattern filters or none.
    let mut examples: Option<Examples> = None;
    let mut q = c.prepare(
        "SELECT at, project_id, level, pattern, query FROM searches
         WHERE query IS NOT NULL OR pattern IS NOT NULL ORDER BY id DESC LIMIT ?1",
    )?;
    let rows = q.query_map([HISTORY], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, Option<i64>>(1)?,
            r.get::<_, Option<i64>>(2)?,
            r.get::<_, Option<i64>>(3)?,
            r.get::<_, Option<String>>(4)?,
        ))
    })?;
    for row in rows {
        if out.iter().all(Option::is_some) {
            break;
        }
        let (at, of, level, pattern, text) = row?;
        if project.is_some() && of.is_some() && of != project {
            continue;
        }
        let found = match text.as_deref() {
            Some(t) => {
                let index = match &mut examples {
                    Some(index) => index,
                    None => examples.insert(Examples::new(targets.iter().map(|t| t.example))?),
                };
                Some(index.found(t))
            }
            None => None,
        };
        for (n, t) in targets.iter().enumerate() {
            let asked = level.is_none_or(|l| t.highest.is_some_and(|h| i64::from(h) >= l))
                && pattern.is_none_or(|p| p.cast_unsigned() == t.id)
                && found.as_ref().is_none_or(|f| f.binary_search(&n).is_ok());
            if asked && out[n].is_none() {
                out[n] = Some(at);
            }
        }
    }
    Ok(out)
}
