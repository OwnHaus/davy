use super::*;

fn store_with_one_event() -> (std::path::PathBuf, Connection) {
    let data = crate::test_support::scratch("query");
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
    assert_eq!(
        r.rows[0][2].as_str().unwrap(),
        format!("{}…[cut]", "x".repeat(4000))
    );
    let whole = run(&c, "SELECT printf('%.4000c', 'y')").unwrap();
    assert_eq!(
        whole.rows[0][0],
        "y".repeat(4000),
        "a cell at the cap is whole"
    );
    assert!(!r.truncated);
    let blob = run(&c, "SELECT payload FROM events").unwrap();
    assert!(blob.rows[0][0]["blob_bytes"].as_u64().unwrap() > 0);
}

#[test]
fn full_text_search_runs_on_the_read_connection() {
    let (_data, c) = store_with_one_event();
    let r = run(
        &c,
        "SELECT i.title FROM issues_text t JOIN issues i ON i.id = t.rowid WHERE issues_text MATCH '\"typeerr\"'",
    )
    .unwrap();
    assert_eq!(r.rows, [vec![json!("TypeError")]]);
    assert!(
        run(&c, "PRAGMA journal_mode").is_err(),
        "other pragmas stay denied"
    );
}

#[test]
fn credential_columns_read_as_null() {
    let (data, _) = store_with_one_event();
    let heartbeat = crate::monitor::NewMonitor::new(
        "Backup",
        crate::monitor::Target::Heartbeat {
            grace: Duration::from_secs(300),
            slug: None,
        },
        None,
    )
    .unwrap();
    crate::monitor::store::add(&store::open(&data).unwrap(), 1, &heartbeat, 0).unwrap();
    let c = open(&data).unwrap();
    let r = run(
        &c,
        "SELECT name, heartbeat_token, length(heartbeat_token), heartbeat_token IS NULL FROM monitors",
    )
    .unwrap();
    assert_eq!(
        r.rows,
        [vec![json!("Backup"), Value::Null, Value::Null, json!(1)]]
    );
    let r = run(
        &c,
        "SELECT count(*) FROM monitors WHERE heartbeat_token LIKE '%'",
    )
    .unwrap();
    assert_eq!(r.rows, [vec![json!(0)]]);
    crate::alert::store::add(
        &store::open(&data).unwrap(),
        None,
        &crate::alert::Destination::Webhook(
            crate::monitor::CheckUrl::parse("https://hooks.example/").unwrap(),
        ),
        false,
        0,
    )
    .unwrap();
    let r = run(&c, "SELECT kind, url, secret, url IS NULL FROM channels").unwrap();
    assert_eq!(
        r.rows,
        [vec![json!("webhook"), Value::Null, Value::Null, json!(1)]]
    );
    let site = crate::monitor::NewMonitor::new(
        "Site",
        crate::monitor::Target::Http {
            url: crate::monitor::CheckUrl::parse("https://shop.example/health?token=x").unwrap(),
            expect: crate::monitor::Expect::Status,
            timeout: Duration::from_secs(5),
        },
        None,
    )
    .unwrap();
    crate::monitor::store::add(&store::open(&data).unwrap(), 1, &site, 0).unwrap();
    let r = run(&c, "SELECT name, url FROM monitors WHERE kind = 'http'").unwrap();
    assert_eq!(r.rows, [vec![json!("Site"), Value::Null]]);
}

#[test]
fn password_session_and_invite_hashes_read_as_null() {
    use crate::accounts::{Email, Secret, store as accounts};
    let (data, _) = store_with_one_event();
    let w = store::open(&data).unwrap();
    let hash = || Secret::random().unwrap().hash();
    let ann = Email::parse("ann@example.com").unwrap();
    let invite = hash();
    accounts::add(&w, &ann, &invite, 0).unwrap().unwrap();
    let welcome = accounts::Change::Welcome {
        invite,
        password: "$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaA".to_owned(),
        session: hash(),
        at: 0,
    };
    assert_eq!(accounts::apply(&w, &welcome).unwrap(), Ok(()));
    let bob = Email::parse("bob@example.com").unwrap();
    accounts::add(&w, &bob, &hash(), 0).unwrap().unwrap();
    let c = open(&data).unwrap();
    let r = run(&c, "SELECT email, password IS NULL FROM users ORDER BY id").unwrap();
    assert_eq!(
        r.rows,
        [
            vec![json!("ann@example.com"), json!(1)],
            vec![json!("bob@example.com"), json!(1)]
        ]
    );
    let r = run(
        &c,
        "SELECT count(*) FROM users WHERE password LIKE '$argon2%'",
    )
    .unwrap();
    assert_eq!(r.rows, [vec![json!(0)]]);
    for table in ["sessions", "invites"] {
        let r = run(&c, &format!("SELECT hash, length(hash) FROM {table}")).unwrap();
        assert_eq!(r.rows, [vec![Value::Null, Value::Null]], "{table}");
    }
    let stored: Option<String> = w
        .query_row(
            "SELECT password FROM users WHERE email = 'ann@example.com'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        stored.is_some(),
        "the hash is there, the agent just does not see it"
    );
}

#[test]
fn a_value_over_16_mib_is_refused() {
    let (_data, c) = store_with_one_event();
    let e = run(&c, "SELECT length(hex(zeroblob(400000000)))").unwrap_err();
    assert!(format!("{e:#}").contains("too big"), "{e:#}");
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
fn payload_json_refuses_output_over_16_mib() {
    let (_data, c) = store_with_one_event();
    let sql = |bytes: usize| {
        let blob = zstd::encode_all(vec![b' '; bytes].as_slice(), 19).unwrap();
        let hex = crate::text::hex(&blob);
        format!("SELECT length(payload_json(x'{hex}'))")
    };
    assert_eq!(run(&c, &sql(1 << 20)).unwrap().rows[0][0], 1 << 20);
    let e = run(&c, &sql((16 << 20) + 1)).unwrap_err();
    assert!(
        e.to_string().contains("payload is larger than 16 MiB"),
        "{e}"
    );
}

#[test]
fn schema_lists_tables_and_notes() {
    let (_data, c) = store_with_one_event();
    let s = schema(&c).unwrap();
    assert!(s.contains("CREATE TABLE releases("));
    assert!(s.contains("CREATE VIRTUAL TABLE issues_text USING fts5("));
    assert!(!s.contains("issues_text_data"));
    assert!(s.contains("payload_json(payload)"));
    assert!(s.contains(
        "issues.event_count, first_seen, last_seen and issue_hourly follow the events kept."
    ));
    assert!(!s.contains("last_seen stays"));
}

#[test]
fn payload_json_reads_a_payload_kept_only_for_being_the_newest() {
    let (data, c) = store_with_one_event();
    // The empty zstd skippable frame that marks such a payload.
    let mut marked = vec![0x5D, 0x2A, 0x4D, 0x18, 0, 0, 0, 0];
    marked.extend(zstd::encode_all(&br#"{"message":"newest"}"#[..], 3).unwrap());
    let w = store::open(&data).unwrap();
    w.execute("UPDATE events SET payload=?1", [&marked])
        .unwrap();
    let r = run(
        &c,
        "SELECT json_extract(payload_json(payload), '$.message') FROM events",
    )
    .unwrap();
    assert_eq!(r.rows, [vec![json!("newest")]]);
}

#[test]
fn reading_a_directory_without_a_database_says_so_and_creates_nothing() {
    let data = crate::test_support::scratch("query-none");
    let err = open(&data).unwrap_err();
    assert_eq!(
        err.to_string(),
        format!(
            "no database in {}: run davy serve --data {} first",
            data.display(),
            data.display()
        )
    );
    assert!(!data.exists(), "the directory stays missing");
}

#[test]
fn reading_a_database_of_an_older_davy_asks_for_an_upgrade_and_leaves_it() {
    let data = crate::test_support::scratch("query-old");
    let c = store::open(&data).unwrap();
    c.execute_batch(crate::usage::UNDO).unwrap();
    let older = store::SCHEMA_VERSION - 1;
    c.pragma_update(None, "user_version", older).unwrap();
    let err = open(&data).unwrap_err();
    assert_eq!(
        err.to_string(),
        format!(
            "the database in {} is from an older Davy: run davy serve --data {} once to upgrade it",
            data.display(),
            data.display()
        )
    );
    let version: u32 = c
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    let tables: i64 = c
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name='usage_daily'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!((version, tables), (older, 0));
}

#[test]
fn reading_a_database_of_a_newer_davy_refuses_it() {
    let data = crate::test_support::scratch("query-new");
    let c = store::open(&data).unwrap();
    let newer = store::SCHEMA_VERSION + 1;
    c.pragma_update(None, "user_version", newer).unwrap();
    let err = open(&data).unwrap_err();
    assert_eq!(
        err.to_string(),
        format!(
            "a newer Davy has upgraded this data directory to schema version {newer}, and this one knows versions up to {}. Run that release or a newer one, or restore the backup taken before the upgrade",
            store::SCHEMA_VERSION
        )
    );
}

fn listing(data: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(data)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn projects(c: &Connection) -> i64 {
    c.query_row("SELECT count(*) FROM projects", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn a_reader_of_a_stopped_servers_directory_sees_what_a_server_writes_later() {
    let data = crate::test_support::scratch("query-closed");
    let c = store::open(&data).unwrap();
    store::create_project(&c, "web").unwrap();
    drop(c);
    let r = open(&data).unwrap();
    assert_eq!(projects(&r), 1);
    let c = store::open(&data).unwrap();
    store::create_project(&c, "shop").unwrap();
    assert_eq!(projects(&r), 2);
    drop((r, c));
    std::fs::remove_dir_all(&data).unwrap();
}

#[cfg(unix)]
#[test]
fn a_stopped_servers_directory_on_a_read_only_mount_is_refused_with_why() {
    use std::os::unix::fs::PermissionsExt as _;
    let data = crate::test_support::scratch("query-mount");
    let c = store::open(&data).unwrap();
    store::create_project(&c, "web").unwrap();
    drop(c);
    let mode = |m| std::fs::set_permissions(&data, std::fs::Permissions::from_mode(m));
    mode(0o555).unwrap();
    let read = open(&data).map(|r| projects(&r));
    let after = listing(&data);
    mode(0o700).unwrap();
    let why = format!("{:#}", read.unwrap_err());
    let want = format!(
        "cannot read the database in {}: no server has it open and the directory is read-only, so SQLite cannot create the davy.sqlite-shm file it reads through; copy the data directory to one you may write, or read it as a user who may write it (",
        data.display()
    );
    assert!(why.starts_with(&want), "{why}");
    assert_eq!(after, ["davy.sqlite"]);
    std::fs::remove_dir_all(&data).unwrap();
}

#[test]
fn a_reader_next_to_a_running_server_sees_what_it_wrote_last() {
    let data = crate::test_support::scratch("query-open");
    let c = store::open(&data).unwrap();
    store::create_project(&c, "web").unwrap();
    // Not checkpointed yet: the row is in the WAL alone.
    store::create_project(&c, "shop").unwrap();
    assert_eq!(projects(&open(&data).unwrap()), 2);
    drop(c);
    std::fs::remove_dir_all(&data).unwrap();
}

#[cfg(unix)]
#[test]
fn a_data_path_that_would_misread_as_a_uri_opens() {
    let root = crate::test_support::scratch("query-path");
    let odd = root.join("my davy?mode=rw#%41");
    store::create_project(&store::open(&odd).unwrap(), "web").unwrap();
    let mut doubled = std::ffi::OsString::from("/");
    doubled.push(&root);
    let doubled = std::path::PathBuf::from(doubled).join("my davy?mode=rw#%41");
    assert!(doubled.as_os_str().as_encoded_bytes().starts_with(b"//"));
    for data in [odd, doubled] {
        assert_eq!(projects(&open(&data).unwrap()), 1, "{}", data.display());
    }
    std::fs::remove_dir_all(&root).unwrap();
}

/// The query the notes give for every stored payload of issue 1.
const PAYLOADS: &str = "SELECT e.id, payload_json(coalesce(e.payload, l.payload)) AS payload FROM events e LEFT JOIN latest_payloads l ON l.issue_id = e.issue_id AND l.event = e.id WHERE e.issue_id = 1";

/// Issue 1 with 52 events, each with its number in `extra.n`, so event 51
/// is outside the sample and the newest, and issue 2 with one event.
fn store_with_a_newest_payload() -> (std::path::PathBuf, Connection) {
    let data = crate::test_support::scratch("query-latest");
    let c = store::open(&data).unwrap();
    store::create_project(&c, "web").unwrap();
    for n in 0..52 {
        let v = json!({"message":"boom","extra":{"n":n}});
        store::insert(&c, 1, &crate::event::ErrorEvent::parse(v).unwrap()).unwrap();
    }
    let other = json!({"message":"other","extra":{"n":"other"}});
    store::insert(&c, 1, &crate::event::ErrorEvent::parse(other).unwrap()).unwrap();
    (data.clone(), open(&data).unwrap())
}

#[test]
fn the_notes_read_the_newest_payload_of_an_issue() {
    let (data, c) = store_with_a_newest_payload();
    assert!(schema(&c).unwrap().contains(PAYLOADS), "the notes changed");
    // A row of issue 2 that names event 51 of issue 1, as one left behind
    // when its event was deleted and the id given again. It is not read.
    let stray = zstd::encode_all(br#"{"extra":{"n":"stray"}}"#.as_slice(), 1).unwrap();
    store::open(&data)
        .unwrap()
        .execute(
            "INSERT INTO latest_payloads(issue_id, event, payload) VALUES (2, 51, ?1)",
            [stray],
        )
        .unwrap();
    let r = run(
        &c,
        &format!("SELECT json_extract(payload, '$.extra.n') FROM ({PAYLOADS}) ORDER BY id"),
    )
    .unwrap();
    let n: Vec<Value> = r.rows.into_iter().map(|mut row| row.remove(0)).collect();
    let expected: Vec<Value> = (0..50)
        .map(|n| json!(n))
        .chain([Value::Null, json!(51)])
        .collect();
    assert_eq!(n, expected);
}

#[test]
fn the_notes_find_the_newest_payload_by_its_key() {
    let (_data, c) = store_with_a_newest_payload();
    let r = run(&c, &format!("EXPLAIN QUERY PLAN {PAYLOADS}")).unwrap();
    let plan: Vec<&Value> = r.rows.iter().map(|row| &row[3]).collect();
    assert_eq!(
        plan,
        [
            "SEARCH e USING INDEX events_issue_timestamp (issue_id=?)",
            "SEARCH l USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN"
        ]
    );
}
