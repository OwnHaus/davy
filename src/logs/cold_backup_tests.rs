//! A backup taken while a day turns cold, and one after: each restores to
//! a log directory that reads as the source did. See "Backup and restore"
//! in docs/design.md.

use super::{
    super::layout,
    tests::{hot, read, record},
};
use crate::backup::{take::take, unpack};
use crate::logs::store::{State, no_overrides};
use crate::test_support::scratch;
use crate::time::{DAY_NS, day_of, midnight, now_ns};
use std::{path::PathBuf, time::Duration};

const HALFWAY: Duration = Duration::from_secs(60);

/// Restores `backup` into a new data directory and returns its logs.
fn restored(backup: &[u8]) -> PathBuf {
    let data = scratch("cold-backup-restored");
    unpack(backup, &data);
    crate::logs::dir(&data)
}

#[test]
fn a_backup_while_a_day_turns_cold_restores_it_hot_and_one_after_cold() {
    let data = scratch("cold-backup");
    drop(crate::store::open(&data).unwrap());
    let dir = crate::logs::dir(&data);
    let day = day_of(now_ns() - 10 * DAY_NS);
    let mut state = State::new(dir.clone(), 30, no_overrides());
    let start = midnight(day) + 3_600_000_000_000;
    let batch = (0..30)
        .map(|i| {
            record(
                1 + i % 2,
                start + i * 1_000_000_007,
                &format!("job {i} done"),
            )
        })
        .collect();
    state.add(batch).unwrap();
    state.commit().unwrap();
    let before = read(&dir);

    let (there, go) = state.cold.pause_next();
    state.start_job(day).unwrap();
    there.recv_timeout(HALFWAY).unwrap();
    assert_eq!(layout::temps(&dir, day).unwrap(), [1]);
    let during = take(&data, Vec::new()).unwrap().0;
    go.send(()).unwrap();
    state.end_job().unwrap();
    assert!(!hot(&dir, day).exists());
    let after = take(&data, Vec::new()).unwrap().0;
    state.close_all().unwrap();

    let mid = restored(&during);
    assert_eq!(read(&mid), before);
    assert!(hot(&mid, day).exists());
    assert_eq!(layout::temps(&mid, day).unwrap(), Vec::<u32>::new());
    assert_eq!(layout::segments(&mid, day).unwrap(), Vec::<u32>::new());

    let end = restored(&after);
    assert_eq!(read(&end), before);
    assert!(!hot(&end, day).exists());
    assert_eq!(layout::segments(&end, day).unwrap(), [1]);
}
