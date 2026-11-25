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
    assert_eq!(disk.gaps.skipped, ["logs: a link or not a directory"]);
    assert_eq!(
        disk.logs_unmeasured(),
        Some(NoForecast::LogsLeftOut(
            "part of logs is left out of the disk walk (disk.partial says what), so the growth of logs is unknown".into()
        ))
    );
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
fn a_walk_stops_after_its_entry_limit_and_says_the_logs_are_not_all_measured() {
    let data = scratch("usage-disk-limit");
    for name in ["a", "b", "c", "d"] {
        file(&data, &format!("sourcemaps/{name}"), 10);
    }
    let limits = |entries| Limits {
        entries,
        ..Limits::default()
    };
    let disk = measure_within(&data, day("2026-11-24"), limits(4)).unwrap();
    assert_eq!(
        disk.gaps.stopped.as_deref(),
        Some("it read 4 entries, its limit")
    );
    assert_eq!(
        disk.logs_unmeasured(),
        Some(NoForecast::Stopped(
            "the disk walk stopped early, it read 4 entries, its limit, so not every log day is measured".into()
        ))
    );
    let whole = measure_within(&data, day("2026-11-24"), limits(5)).unwrap();
    assert_eq!((whole.logs_unmeasured(), whole.gaps.stopped), (None, None));
    fs::remove_dir_all(&data).unwrap();
}

#[test]
fn a_walk_stops_at_its_time_limit() {
    let data = scratch("usage-disk-time");
    file(&data, "logs/2026-11-23/a", 1_000);
    let limits = Limits {
        time: Duration::ZERO,
        ..Limits::default()
    };
    let disk = measure_within(&data, day("2026-11-24"), limits).unwrap();
    assert_eq!(
        disk.gaps.stopped.as_deref(),
        Some("it took more than 0 s, its limit")
    );
    fs::remove_dir_all(&data).unwrap();
}

/// Makes `path` under `data` a directory nobody may read, as ext4 makes
/// `lost+found` for root alone.
#[cfg(unix)]
fn locked(data: &Path, path: &str) {
    use std::os::unix::fs::PermissionsExt as _;
    file(data, &format!("{path}/inside"), 100_000);
    fs::set_permissions(data.join(path), fs::Permissions::from_mode(0o000)).unwrap();
}

#[cfg(unix)]
fn unlock(data: &Path, path: &str) {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(data.join(path), fs::Permissions::from_mode(0o700)).unwrap();
}

#[cfg(unix)]
#[test]
fn a_directory_davy_may_not_read_is_left_out_and_named() {
    let data = scratch("usage-disk-locked");
    file(&data, "logs/2026-11-23/a", 100_000);
    locked(&data, "lost+found");
    let disk = measure(&data, day("2026-11-24"));
    unlock(&data, "lost+found");
    let disk = disk.unwrap();
    assert_eq!(disk.gaps.skipped, ["lost+found: permission denied"]);
    assert_eq!((disk.gaps.skipped_count, disk.logs_gap), (1, false));
    assert_eq!(disk.logs_unmeasured(), None);
    assert_eq!(disk.closed.len(), 1);
    // The directory's own block counts, the file inside does not.
    assert!(disk.other < 100_000, "{disk:?}");
    fs::remove_dir_all(&data).unwrap();
}

#[cfg(unix)]
#[test]
fn a_log_day_davy_may_not_read_leaves_the_growth_unknown() {
    let data = scratch("usage-disk-locked-day");
    file(&data, "logs/2026-11-22/a", 100_000);
    locked(&data, "logs/2026-11-23");
    let disk = measure(&data, day("2026-11-24"));
    unlock(&data, "logs/2026-11-23");
    let disk = disk.unwrap();
    assert_eq!(disk.gaps.skipped, ["logs/2026-11-23: permission denied"]);
    assert!(disk.logs_unmeasured().is_some(), "{disk:?}");
    fs::remove_dir_all(&data).unwrap();
}

#[test]
fn a_walk_names_what_lies_deeper_than_it_goes() {
    let data = scratch("usage-disk-deep");
    let deep = vec!["d"; 34].join("/");
    file(&data, &format!("{deep}/a"), 1_000);
    let disk = measure(&data, day("2026-11-24")).unwrap();
    let cut = vec!["d"; 33].join("/");
    assert_eq!(
        (disk.gaps.skipped, disk.gaps.skipped_count, disk.logs_gap),
        (
            vec![format!("{cut}: more than 32 directories deep")],
            1,
            false
        )
    );
    fs::remove_dir_all(&data).unwrap();
}

#[cfg(unix)]
#[test]
fn a_fifo_is_sized_without_being_opened() {
    let data = scratch("usage-disk-fifo");
    fs::create_dir_all(&data).unwrap();
    // Opening a FIFO with no writer would block the walk for good.
    nix::unistd::mkfifo(&data.join("pipe"), nix::sys::stat::Mode::S_IRWXU).unwrap();
    let disk = measure(&data, day("2026-11-24")).unwrap();
    assert_eq!((disk.other, disk.gaps.is_empty()), (0, true));
    fs::remove_dir_all(&data).unwrap();
}
