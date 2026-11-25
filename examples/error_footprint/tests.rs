use super::*;

/// A fresh path under `target/`, not created yet.
fn scratch(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join(format!("test-footprint-{name}-{}", uuid::Uuid::new_v4()))
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(ToString::to_string).collect()
}

#[test]
fn a_count_of_zero_is_refused() {
    for name in ["ISSUES", "PER_ISSUE", "PER_TX", "ROUNDS"] {
        let e = count(name, "0").unwrap_err();
        assert_eq!(e.to_string(), format!("{name} must be above 0"));
    }
    let e = count("PER_TX", "many").unwrap_err();
    assert_eq!(
        e.to_string(),
        "PER_TX must be a whole number above 0, not many"
    );
    assert_eq!(count("PER_TX", "64").unwrap(), 64);
}

#[test]
fn a_run_refuses_zero_before_it_writes() {
    let data = scratch("zero");
    let path = data.to_string_lossy();
    let e = run(None, &args(&[&path, "0", "26", "64"])).unwrap_err();
    assert_eq!(e.to_string(), "ISSUES must be above 0");
    let e = run(Some(Design::Table), &args(&[&path, "10", "26", "0"])).unwrap_err();
    assert_eq!(e.to_string(), "PER_TX must be above 0");
    for (issues, per_issue) in [("4294967296", "4294967296"), ("1000000", "1001")] {
        let e = run(None, &args(&[&path, issues, per_issue, "64"])).unwrap_err();
        assert_eq!(
            e.to_string(),
            "ISSUES times PER_ISSUE must be at most 1000000000"
        );
    }
    assert!(!data.exists());
    let e = reads::compare(&args(&["old", "new", "0"])).unwrap_err();
    assert_eq!(e.to_string(), "ROUNDS must be above 0");
}

#[test]
fn upgrade_refuses_a_store_outside_a_scratch_directory() {
    // The root, which no checkout lies under, wherever this one is.
    let e = upgrade(Path::new("/")).unwrap_err();
    assert_eq!(
        e.to_string(),
        "upgrade migrates / in place, and no older Davy opens it after. Copy the data directory under a .scratch directory and pass the copy"
    );
    assert!(!is_copy(Path::new("/var/lib/davy")));
    assert!(is_copy(Path::new("/home/a/davy/.scratch/copy")));
}

/// A store of one issue of 51 events, written by the real writer.
fn hot(data: &Path) {
    let mut real = Real::open(data).unwrap();
    let events = (0..51)
        .map(|n| ErrorEvent::parse(json!({"message": "boom", "extra": {"n": n}})).unwrap())
        .collect();
    real.write(events).unwrap();
    drop(real);
}

/// A store one version behind, as the version before `latest_payloads`
/// left it: one issue of 51 events, the newest one's payload in its events
/// row behind the mark.
fn behind(data: &Path) {
    hot(data);
    let c = Connection::open(data.join(store::DB_FILE)).unwrap();
    let payload: Vec<u8> = c
        .query_row("SELECT payload FROM latest_payloads", [], |r| r.get(0))
        .unwrap();
    let marked = [LATEST.as_slice(), &payload].concat();
    c.execute("UPDATE events SET payload=?1 WHERE id=51", [marked])
        .unwrap();
    let version: i64 = c
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    c.execute_batch(&format!(
        "DROP TABLE latest_payloads; PRAGMA user_version={};",
        version - 1
    ))
    .unwrap();
}

#[test]
fn upgrade_migrates_a_copy_under_a_scratch_directory() {
    let root = scratch("upgrade");
    let data = root.join(".scratch").join("copy");
    behind(&data);
    upgrade(&data).unwrap();
    let c = Connection::open(data.join(store::DB_FILE)).unwrap();
    let moved: (i64, i64) = c
        .query_row(
            "SELECT (SELECT event FROM latest_payloads), (SELECT count(*) FROM events
             WHERE substr(payload, 1, 8)=x'5D2A4D1800000000')",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(moved, (51, 0));
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn reads_runs_the_payload_queries_of_both_layouts() {
    let root = scratch("reads");
    let (old, new) = (root.join("old"), root.join("new"));
    behind(&old);
    hot(&new);
    let (old, new) = (old.to_string_lossy(), new.to_string_lossy());
    reads::compare(&args(&[&old, &new, "1"])).unwrap();
    std::fs::remove_dir_all(&root).unwrap();
}

#[cfg(unix)]
#[test]
fn a_sim_store_is_readable_by_its_owner_only() {
    use std::os::unix::fs::PermissionsExt as _;
    let data = scratch("sim");
    Sim::open(&data, Design::Table, 1).unwrap();
    let mode = std::fs::metadata(&data).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o700);
    std::fs::remove_dir_all(&data).unwrap();
}
