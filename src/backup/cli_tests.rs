//! The backup command's own files beside the backup.

use super::*;
use crate::test_support::scratch;

#[test]
fn a_killed_backups_temporary_file_goes_with_the_next_backup_to_that_name() {
    let dir = scratch("backup-out");
    fs::create_dir_all(&dir).unwrap();
    for name in [
        ".out.davy.tmp-1",
        ".out.davy.tmp-2",
        ".out.davy.tmp-x",
        ".other.davy.tmp-3",
    ] {
        fs::write(dir.join(name), "half").unwrap();
    }
    // A backup to the same name that still runs holds its file.
    let running = File::open(dir.join(".out.davy.tmp-2")).unwrap();
    running.lock().unwrap();
    leftovers(&dir, "out.davy");
    let mut left: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    left.sort();
    assert_eq!(
        left,
        [".other.davy.tmp-3", ".out.davy.tmp-2", ".out.davy.tmp-x"]
    );
}
