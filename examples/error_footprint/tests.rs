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

#[test]
fn upgrade_migrates_a_copy_under_a_scratch_directory() {
    let root = scratch("upgrade");
    let data = root.join(".scratch").join("copy");
    std::fs::create_dir_all(&data).unwrap();
    upgrade(&data).unwrap();
    let c = Connection::open(data.join(store::DB_FILE)).unwrap();
    let rows: i64 = c
        .query_row("SELECT count(*) FROM latest_payloads", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 0);
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
