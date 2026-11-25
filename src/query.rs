//! Read-only SQL over the error store, for agents and `davy sql`.

use crate::store;
use anyhow::{Result, bail};
use rusqlite::{
    Connection, OpenFlags,
    functions::FunctionFlags,
    hooks::{AuthAction, AuthContext, Authorization},
    limits::Limit,
    types::ValueRef,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::{io::Read as _, path::Path, sync::mpsc, thread, time::Duration};

pub const MAX_ROWS: usize = 500;
const MAX_TEXT_CHARS: usize = 4000;
/// The longest string or blob a query may build, in bytes.
const MAX_LENGTH: i32 = 16 * 1024 * 1024;
/// Ends a text cell cut at [`MAX_TEXT_CHARS`], so an agent does not take it
/// for the whole value.
const CUT: &str = "…[cut]";
const TIMEOUT: Duration = Duration::from_secs(5);
/// A stored payload is at most 1 MiB of JSON, so this only stops a crafted blob.
const MAX_PAYLOAD: u64 = 16 * 1024 * 1024;

/// Whether a read-only open of the WAL database `path` cannot work: its
/// `-wal` and `-shm` files are both missing, as SQLite deletes them when
/// the last connection closes, and the reader may not write the
/// directory, as on a read-only mount, so SQLite cannot create `-shm`.
///
/// Davy never reads such a database as `immutable=1`, which would work
/// there: nothing stops a server, perhaps as another user or through
/// another mount, from opening the database a moment later, and an
/// immutable reader then mixes old and new pages without an error.
fn cannot_open_read_only(path: &Path) -> bool {
    let side = |ext: &str| {
        let mut name = path.as_os_str().to_owned();
        name.push(ext);
        std::path::PathBuf::from(name).exists()
    };
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    !side("-wal") && !side("-shm") && !writable(dir)
}

#[cfg(unix)]
fn writable(dir: &Path) -> bool {
    nix::unistd::access(dir, nix::unistd::AccessFlags::W_OK).is_ok()
}

#[cfg(not(unix))]
fn writable(_: &Path) -> bool {
    true
}

/// What an agent needs to write a query, beyond the table definitions.
const NOTES: &str = "Notes:
- Times in projects, issues, events and releases are unix milliseconds. issue_hourly.hour is the start of its hour in unix milliseconds.
- events.payload is the compressed Sentry event, kept for a sample of each issue's events: an event keeps it when its issue kept fewer than 50 events before it, or when it is the issue's 100th, 200th, 300th kept event and so on. Events retention deleted do not count, so after retention the sample is no longer the first 50 and every 100th received. The newest event of an issue keeps its payload in latest_payloads.payload when it is outside the sample, with latest_payloads.issue_id its issue and latest_payloads.event its events.id. Join on both, so SQLite finds the row by its key: SELECT e.id, payload_json(coalesce(e.payload, l.payload)) AS payload FROM events e LEFT JOIN latest_payloads l ON l.issue_id = e.issue_id AND l.event = e.id WHERE e.issue_id = 1 reads every stored payload of issue 1. payload_json(payload) returns a payload as JSON text, so json_extract(payload_json(payload), '$.exception.values[0].type') works.
- projects.own is 1 for Davy's own project, which holds Davy's own warnings, errors and crashes, and 0 for every project a person made. project_status names its slug under davy.project.
- issues.status is unresolved, resolved or ignored. A resolved issue reopens when an event arrives without a release, in the release it was resolved in, or in a release first seen after resolved_at.
- projects.log_retention_days and projects.error_retention_days are the days of logs and errors a project keeps, NULL for the server default; project_status shows the days in effect. Events and user feedback received before that are deleted. issues.event_count, first_seen, last_seen and issue_hourly follow the events kept. An issue with no event left is deleted once no user feedback points at it, and has event_count 0 until then. A new issue never takes a deleted issue's id: id_floor holds the highest id deleted.
- issue_tags holds each environment and release that an issue's kept events named, and in last_at when the newest of them was received. issues_text is a trigram full-text index of issues.title and issues.culprit: SELECT rowid FROM issues_text WHERE issues_text MATCH '\"timed out\"' finds issues whose title or culprit contains the phrase, ignoring case, for phrases of three characters or more.
- monitors.state is pending, up, failing or down. Times in monitors, checks and incidents are unix milliseconds. An incident with no resolved_at is open. monitors.heartbeat_token, monitors.url, channels.url, channels.secret and projects.vercel_secret read as NULL.
- channels.dropped counts issue, feedback and operator notices that a full outbox dropped, so above 0 a real alert was lost. channels.sdk_dropped counts dropped notices of monitors made by check-ins, which anyone with a project's DSN can raise.
- A heartbeat with monitors.slug also takes Sentry SDK check-ins for that slug. monitors.source is cli for a monitor made by davy monitor add and sdk for one made by a check-in. A heartbeat expects every every_s seconds, or every months calendar months, or at the times of the crontab cron, both in the IANA zone; due_at is when the sweep marks it down. run_id and run_started_at belong to a run started by an in_progress check-in. Refused check-ins are counted in client_reports with category monitor and reason davy_too_large, davy_invalid, davy_bad_slug, davy_unknown_monitor, davy_bad_config, davy_monitor_cap or davy_rate.
- users.password, sessions.hash and invites.hash read as NULL.
- client_reports counts what the SDKs dropped before sending, per UTC day (unix milliseconds at midnight), project, reason and category, such as before_send and error, or sample_rate and transaction. reason davy_daily_cap counts user feedback Davy refused past 1000 per project and day, and with category session the sessions refused past 1000000 per project and day. reason davy_out_of_window counts sessions that started more than 5 days ago or over an hour ahead. Rows are kept 90 days. When an app reports no errors, check here that its SDK did not drop them.
- release_sessions counts the sessions SDKs report, per project, hour the session started (unix milliseconds), release and environment. started is every session, errored the ones with an error (crashed ones too), crashed and abnormal the ones that ended so. Crash-free sessions are 100 * (started - crashed) / started. release_users holds one row per UTC day, project, release, environment and user, where user is a hash of the SDK's user id and flags has bit 2 for errored and bit 4 for crashed. Both are kept 90 days. The release_health tool reads them. session_seen is how Davy counts each session once.
- user_feedback holds what users wrote through the SDK's feedback widget or captureFeedback. received_at is unix milliseconds. issue_id is the issue of the event it names, NULL when it names none, arrived before that event, or that issue was deleted.
- log_days holds, per UTC day of the log index (YYYY-MM-DD text) and project, the log records stored and their raw bytes as the pattern counters count them. cut is 1 once retention deleted logs of that day and project, and stays 1. The firsts row with what log_days says when counting started, in unix milliseconds: days before it are not counted, and its own day only in part. store_days holds, once a UTC day, at: when it was sampled in unix milliseconds, bytes: what SQLite's live pages take, and events: the events kept. Both are kept 400 days. The usage tool reads log_days.
- issues.discarded_at is when a person discarded the issue with davy issue discard, unix milliseconds, NULL when it is not discarded. issue_context returns it as discarded_at. discarded_daily counts, per UTC day of receipt (YYYY-MM-DD text) and project, the events of discarded issues that were counted and not stored, kept 400 days. It counts events received: an SDK's resend of one event counts again.
- Results stop at 500 rows, text at 4000 characters, payload_json at 16 MiB, and a query at 5 seconds.";

#[derive(Debug, Serialize)]
pub struct Rows {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
    /// True when the query had more than `MAX_ROWS` rows.
    pub truncated: bool,
}

/// Opens the store read-only. It neither creates nor upgrades the
/// database: a directory without one, or with one another Davy wrote, is
/// an error that says what to run. In a directory the reader may write, a
/// read-only open of a WAL database creates its `-wal` and `-shm` files
/// when they are missing and leaves them. Where it cannot, it is an error
/// that says why; see [`cannot_open_read_only`].
pub fn open(data: &Path) -> Result<Connection> {
    let path = data.join(store::DB_FILE);
    if !path.exists() {
        bail!(
            "no database in {}: run davy serve --data {} first",
            data.display(),
            data.display()
        );
    }
    let mut c = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    c.busy_timeout(store::BUSY_TIMEOUT)?;
    let version: u32 = match c.query_row("PRAGMA user_version", [], |r| r.get(0)) {
        Ok(v) => v,
        Err(e) if cannot_open_read_only(&path) => bail!(
            "cannot read the database in {}: no server has it open and the directory is read-only, so SQLite cannot create the {}-shm file it reads through; copy the data directory to one you may write, or read it as a user who may write it ({e})",
            data.display(),
            store::DB_FILE
        ),
        Err(e) => return Err(e.into()),
    };
    // The same refusal as a writer's: reading what this build does not
    // know would answer from tables a newer Davy may have changed.
    store::refuse_newer(version)?;
    if version < store::SCHEMA_VERSION {
        bail!(
            "the database in {} is from an older Davy: run davy serve --data {} once to upgrade it",
            data.display(),
            data.display()
        );
    }
    // SQLite's default of 1 GB lets one `hex(zeroblob(..))` take that much
    // memory, and the timeout cannot stop a single allocation.
    c.set_limit(Limit::SQLITE_LIMIT_LENGTH, MAX_LENGTH)?;
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
            let failed =
                |e: Box<dyn std::error::Error + Send + Sync>| rusqlite::Error::UserFunctionError(e);
            let mut json = Vec::new();
            zstd::stream::Decoder::new(blob.as_slice())
                .map_err(|e| failed(e.into()))?
                .take(MAX_PAYLOAD + 1)
                .read_to_end(&mut json)
                .map_err(|e| failed(e.into()))?;
            if json.len() as u64 > MAX_PAYLOAD {
                return Err(failed("payload is larger than 16 MiB".into()));
            }
            String::from_utf8(json)
                .map(Some)
                .map_err(|e| failed(e.into()))
        },
    )?;
    Ok(c)
}

/// Columns that hold a credential. An agent has no use for one, and a
/// prompt-injected agent could leak it, so they read as NULL. A monitor URL
/// often carries a token in its query string. A password hash can be cracked
/// offline, and the session and invite hashes are kept out the same way.
const CREDENTIALS: [(&str, &str); 8] = [
    ("monitors", "heartbeat_token"),
    ("monitors", "url"),
    ("channels", "url"),
    ("channels", "secret"),
    ("users", "password"),
    ("sessions", "hash"),
    ("invites", "hash"),
    ("projects", "vercel_secret"),
];

/// SQLite counts ATTACH and most PRAGMAs as read-only, so `Statement::readonly`
/// is not enough. This allows a query to select, read tables and call
/// functions, and nothing else. The one pragma allowed is `data_version`,
/// which FTS5 reads to notice another connection's writes to issues_text.
fn only_reads(ctx: AuthContext<'_>) -> Authorization {
    match ctx.action {
        AuthAction::Read {
            table_name,
            column_name,
        } if CREDENTIALS.contains(&(table_name, column_name)) => Authorization::Ignore,
        AuthAction::Select
        | AuthAction::Read { .. }
        | AuthAction::Function { .. }
        | AuthAction::Recursive
        | AuthAction::Pragma {
            pragma_name: "data_version",
            pragma_value: None,
        } => Authorization::Allow,
        _ => Authorization::Deny,
    }
}

/// The `CREATE` statements of every table and index, then `NOTES`.
pub fn schema(c: &Connection) -> Result<String> {
    // The shadow tables and triggers of issues_text only keep FTS5 in step.
    let mut q = c.prepare(
        "SELECT sql FROM sqlite_schema WHERE sql IS NOT NULL AND name NOT GLOB 'issues_text_*'",
    )?;
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
        ValueRef::Text(t) => {
            let text = String::from_utf8_lossy(t);
            let mut cell: String = text.chars().take(MAX_TEXT_CHARS).collect();
            if cell.len() < text.len() {
                cell.push_str(CUT);
            }
            cell.into()
        }
        ValueRef::Blob(b) => json!({ "blob_bytes": b.len() }),
    }
}

#[cfg(test)]
#[path = "query_tests.rs"]
mod tests;
