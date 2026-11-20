//! Read-only SQL over the error store, for agents and `davy sql`.

use crate::store;
use anyhow::Result;
use rusqlite::{
    Connection, OpenFlags,
    functions::FunctionFlags,
    hooks::{AuthAction, AuthContext, Authorization},
    types::ValueRef,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::{path::Path, sync::mpsc, thread, time::Duration};

pub const MAX_ROWS: usize = 500;
const MAX_TEXT_CHARS: usize = 4000;
const TIMEOUT: Duration = Duration::from_secs(5);

/// What an agent needs to write a query, beyond the table definitions.
const NOTES: &str = "Notes:
- Times in projects, issues, events and releases are unix milliseconds. issue_hourly.hour is unix hours.
- events.payload is the compressed Sentry event, kept for a sample of events. payload_json(payload) returns it as JSON text, so json_extract(payload_json(payload), '$.exception.values[0].type') works.
- issues.status is unresolved, resolved or ignored. A resolved issue reopens when an event arrives without a release, in the release it was resolved in, or in a release first seen after resolved_at.
- Results stop at 500 rows, text at 4000 characters, and a query at 5 seconds.";

#[derive(Debug, Serialize)]
pub struct Rows {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
    /// True when the query had more than `MAX_ROWS` rows.
    pub truncated: bool,
}

/// Opens the store read-only. Creates the schema first, so a fresh directory
/// answers queries instead of failing on missing tables.
pub fn open(data: &Path) -> Result<Connection> {
    drop(store::open(data)?);
    let mut c = Connection::open_with_flags(
        data.join("davy.sqlite"),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    c.busy_timeout(Duration::from_secs(5))?;
    c.pragma_update(None, "query_only", true)?;
    c.authorizer(Some(only_reads))?;
    c.create_scalar_function(
        "payload_json",
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            let Some(blob) = ctx.get::<Option<Vec<u8>>>(0)? else {
                return Ok(None);
            };
            let json = zstd::stream::decode_all(blob.as_slice())
                .map_err(|e| rusqlite::Error::UserFunctionError(e.into()))?;
            String::from_utf8(json)
                .map(Some)
                .map_err(|e| rusqlite::Error::UserFunctionError(e.into()))
        },
    )?;
    Ok(c)
}

/// SQLite counts ATTACH and most PRAGMAs as read-only, so `Statement::readonly`
/// is not enough. This allows a query to select, read tables and call
/// functions, and nothing else.
fn only_reads(ctx: AuthContext<'_>) -> Authorization {
    match ctx.action {
        AuthAction::Select
        | AuthAction::Read { .. }
        | AuthAction::Function { .. }
        | AuthAction::Recursive => Authorization::Allow,
        _ => Authorization::Deny,
    }
}

/// The `CREATE` statements of every table and index, then `NOTES`.
pub fn schema(c: &Connection) -> Result<String> {
    let mut q = c.prepare("SELECT sql FROM sqlite_schema WHERE sql IS NOT NULL")?;
    let mut out = q
        .query_map([], |r| r.get::<_, String>(0))?
        .map(|sql| sql.map(|s| s + ";\n"))
        .collect::<rusqlite::Result<String>>()?;
    out.push('\n');
    out.push_str(NOTES);
    Ok(out)
}

/// Runs one read-only statement. Errors name what was wrong with the query.
pub fn run(c: &Connection, sql: &str) -> Result<Rows> {
    let mut stmt = c.prepare(sql)?;
    let columns: Vec<String> = stmt.column_names().map(str::to_owned).collect();
    let interrupt = c.get_interrupt_handle();
    let (finished, cancel) = mpsc::channel::<()>();
    let watchdog = thread::spawn(move || {
        if cancel.recv_timeout(TIMEOUT) == Err(mpsc::RecvTimeoutError::Timeout) {
            interrupt.interrupt();
        }
    });
    let result = collect(&mut stmt, columns.len());
    drop(finished);
    watchdog
        .join()
        .map_err(|_panic| anyhow::anyhow!("query watchdog panicked"))?;
    let (rows, truncated) = result.map_err(|e| match e.sqlite_error_code() {
        Some(rusqlite::ErrorCode::OperationInterrupted) => {
            anyhow::anyhow!("query stopped after {} seconds", TIMEOUT.as_secs())
        }
        _ => e.into(),
    })?;
    Ok(Rows {
        columns,
        rows,
        truncated,
    })
}

/// Up to `MAX_ROWS` rows, and whether more were left.
fn collect(
    stmt: &mut rusqlite::Statement<'_>,
    width: usize,
) -> rusqlite::Result<(Vec<Vec<Value>>, bool)> {
    let mut cursor = stmt.query([])?;
    let mut rows = Vec::new();
    while let Some(row) = cursor.next()? {
        if rows.len() == MAX_ROWS {
            return Ok((rows, true));
        }
        rows.push(
            (0..width)
                .map(|i| row.get_ref(i).map(value))
                .collect::<rusqlite::Result<_>>()?,
        );
    }
    Ok((rows, false))
}

fn value(v: ValueRef<'_>) -> Value {
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(n) => n.into(),
        ValueRef::Real(x) => serde_json::Number::from_f64(x).map_or(Value::Null, Value::Number),
        ValueRef::Text(t) => String::from_utf8_lossy(t)
            .chars()
            .take(MAX_TEXT_CHARS)
            .collect::<String>()
            .into(),
        ValueRef::Blob(b) => json!({ "blob_bytes": b.len() }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with_one_event() -> (std::path::PathBuf, Connection) {
        let data = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("target/test-query-{}", uuid::Uuid::new_v4()));
        let c = store::open(&data).unwrap();
        store::create_project(&c, "web").unwrap();
        let event = crate::event::ErrorEvent::parse(
            json!({"message":"boom","exception":[{"type":"TypeError"}],"extra":{"long":"x".repeat(5000)}}),
        )
        .unwrap();
        store::insert(&c, 1, &event).unwrap();
        (data.clone(), open(&data).unwrap())
    }

    #[test]
    fn run_returns_rows_with_payload_json_and_blob_sizes() {
        let (_data, c) = store_with_one_event();
        let r = run(
            &c,
            "SELECT json_extract(payload_json(payload), '$.exception[0].type') AS ty, length(payload) > 0 AS stored, json_extract(payload_json(payload), '$.extra.long') AS long FROM events",
        )
        .unwrap();
        assert_eq!(r.columns, ["ty", "stored", "long"]);
        assert_eq!(r.rows[0][0], "TypeError");
        assert_eq!(r.rows[0][1], 1);
        assert_eq!(r.rows[0][2].as_str().unwrap().len(), 4000);
        assert!(!r.truncated);
        let blob = run(&c, "SELECT payload FROM events").unwrap();
        assert!(blob.rows[0][0]["blob_bytes"].as_u64().unwrap() > 0);
    }

    #[test]
    fn run_stops_at_the_row_cap() {
        let (_data, c) = store_with_one_event();
        let r = run(
            &c,
            "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n LIMIT 501) SELECT x FROM n",
        )
        .unwrap();
        assert_eq!(r.rows.len(), 500);
        assert!(r.truncated);
    }

    #[test]
    fn run_refuses_anything_but_one_reading_statement() {
        let (_data, c) = store_with_one_event();
        for sql in [
            "DELETE FROM issues",
            "UPDATE issues SET status='ignored'",
            "SELECT 1; DELETE FROM issues",
            "ATTACH 'other.sqlite' AS other",
            "PRAGMA query_only=0",
            "",
        ] {
            assert!(run(&c, sql).is_err(), "{sql:?}");
        }
        assert_eq!(
            run(&c, "SELECT count(*) FROM issues").unwrap().rows[0][0],
            1
        );
    }

    #[test]
    fn run_stops_a_slow_query() {
        let (_data, c) = store_with_one_event();
        let e = run(
            &c,
            "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n) SELECT count(*) FROM n",
        )
        .unwrap_err();
        assert_eq!(e.to_string(), "query stopped after 5 seconds");
    }

    #[test]
    fn schema_lists_tables_and_notes() {
        let (_data, c) = store_with_one_event();
        let s = schema(&c).unwrap();
        assert!(s.contains("CREATE TABLE releases("));
        assert!(s.contains("payload_json(payload)"));
    }
}
