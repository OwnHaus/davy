use super::*;
use crate::test_support::scratch;
use std::fs;

fn day(s: &str) -> Date {
    s.parse().unwrap()
}

/// Writes `bytes` bytes to `path` under `data`, with its directories.
fn file(data: &Path, path: &str, bytes: usize) {
    let path = data.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, vec![1_u8; bytes]).unwrap();
}

#[test]
fn the_disk_splits_into_log_days_sqlite_uploads_and_the_rest() {
    let data = scratch("usage-disk");
    file(&data, "logs/2026-11-22/a", 100_000);
    file(&data, "logs/2026-11-23/a", 200_000);
    file(&data, "logs/2026-11-24/a", 50_000);
    file(&data, "logs/.trash/x", 10_000);
    file(&data, "davy.sqlite", 30_000);
    file(&data, "davy.sqlite-wal", 30_000);
    file(&data, "sourcemaps/b/map", 20_000);
    file(&data, "chunks/c", 20_000);
    file(&data, "setup.json", 1_000);
    let disk = measure(&data, day("2026-11-24")).unwrap();
    let days: Vec<Date> = disk.closed.iter().map(|(d, _)| *d).collect();
    assert_eq!(days, [day("2026-11-22"), day("2026-11-23")]);
    // Allocated blocks round each file and directory up, so compare to
    // the bytes written with the rounding allowed.
    let near = |got: u64, want: u64| got >= want && got < want + 40_000;
    assert!(near(disk.closed[0].1, 100_000), "{disk:?}");
    assert!(near(disk.closed[1].1, 200_000), "{disk:?}");
    assert!(near(disk.today, 50_000), "{disk:?}");
    assert!(near(disk.sqlite, 60_000), "{disk:?}");
    assert!(near(disk.uploads, 40_000), "{disk:?}");
    assert!(near(disk.other, 11_000), "{disk:?}");
    assert!(disk.total > disk.free && disk.free > 0, "{disk:?}");
    fs::remove_dir_all(&data).unwrap();
}

#[cfg(unix)]
#[test]
fn a_logs_link_to_another_directory_is_not_walked() {
    let data = scratch("usage-disk-link");
    let outside = scratch("usage-disk-outside");
    file(&outside, "2026-11-23/a", 100_000);
    fs::create_dir_all(&data).unwrap();
    std::os::unix::fs::symlink(&outside, data.join(logs::DIR)).unwrap();
    let disk = measure(&data, day("2026-11-24")).unwrap();
    assert_eq!((disk.closed.as_slice(), disk.today), (&[][..], 0));
    // The link itself takes at most a block.
    assert!(disk.other < 8_192, "{disk:?}");
    fs::remove_dir_all(&data).unwrap();
    fs::remove_dir_all(&outside).unwrap();
}

#[test]
fn a_file_named_like_a_day_is_not_a_closed_day() {
    let data = scratch("usage-disk-file-day");
    file(&data, "logs/2026-11-22/a", 1_000);
    file(&data, "logs/2026-11-23", 100_000);
    let disk = measure(&data, day("2026-11-24")).unwrap();
    let days: Vec<Date> = disk.closed.iter().map(|(d, _)| *d).collect();
    assert_eq!(days, [day("2026-11-22")]);
    assert!(disk.other >= 100_000, "{disk:?}");
    fs::remove_dir_all(&data).unwrap();
}

#[test]
fn a_walk_stops_after_its_entry_limit() {
    let data = scratch("usage-disk-limit");
    for name in ["a", "b", "c", "d"] {
        file(&data, &format!("sourcemaps/{name}"), 10);
    }
    let err = measure_at_most(&data, day("2026-11-24"), 4).unwrap_err();
    assert_eq!(
        err.to_string(),
        "the data directory has more than 4 entries"
    );
    assert!(measure_at_most(&data, day("2026-11-24"), 5).is_ok());
    fs::remove_dir_all(&data).unwrap();
}
