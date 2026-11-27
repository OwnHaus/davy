use super::{super::layout, Verified, cold_day, verify};
use crate::logs::{LogLevel, LogRecord, LogSource};
use crate::test_support::scratch;
use crate::time::{DAY_NS, day_of, midnight, now_ns};
use std::os::unix::fs::FileExt;

fn record(project_id: i64, timestamp: i64) -> LogRecord {
    LogRecord {
        project_id,
        timestamp,
        observed_at: now_ns(),
        source: LogSource::Otlp,
        level: LogLevel::Info,
        level_unknown: false,
        message: format!("job {timestamp} done"),
        service: None,
        trace_id: None,
        span_id: None,
        attributes: serde_json::Map::new(),
    }
}

#[test]
fn a_flipped_bit_or_a_cut_file_is_named_and_the_rest_checked() {
    let dir = scratch("cold-verify");
    assert_eq!(verify(&dir).unwrap(), Verified::default(), "no cold tier");
    let day = day_of(now_ns() - 10 * DAY_NS);
    let logs = (0..40).map(|i| record(1 + i % 2, midnight(day) + i));
    assert_eq!(cold_day(&dir, logs.collect()).unwrap(), day);

    let seg = layout::segment_dir(&dir, day, 1);
    let one = seg.join("1.dlc");
    let two = seg.join("2.dlc");
    let size = |p| std::fs::metadata(p).unwrap().len();
    let whole = size(&one) + size(&two);
    let good = verify(&dir).unwrap();
    assert_eq!(good.days, [(day, whole)]);
    assert_eq!((good.files, good.blocks, good.bad_count), (2, 2, 0));
    assert_eq!(good.bytes(), whole);

    // A bit of the first block's frame, past the 12-byte head and its
    // 25-byte header.
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&one)
        .unwrap();
    let mut b = [0];
    f.read_exact_at(&mut b, 40).unwrap();
    f.write_all_at(&[b[0] ^ 1], 40).unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&two)
        .unwrap()
        .set_len(size(&two) - 3)
        .unwrap();
    let bad = verify(&dir).unwrap();
    assert_eq!((bad.files, bad.blocks, bad.bad_count), (2, 1, 2));
    assert_eq!(
        bad.bad,
        [
            format!("{}: block 0's checksum does not match", one.display()),
            format!("{}: not a cold file", two.display()),
        ]
    );
    assert_eq!(bad.days, [(day, whole - 3)]);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_file_under_another_projects_name_is_named() {
    let dir = scratch("cold-verify-project");
    let day = day_of(now_ns() - 10 * DAY_NS);
    let logs = (0..40).map(|i| record(1 + i % 2, midnight(day) + i));
    cold_day(&dir, logs.collect()).unwrap();
    let seg = layout::segment_dir(&dir, day, 1);
    let nine = layout::file(&seg, 9);
    std::fs::copy(layout::file(&seg, 1), &nine).unwrap();
    let found = verify(&dir).unwrap();
    assert_eq!((found.files, found.blocks, found.bad_count), (3, 2, 1));
    assert_eq!(
        found.bad,
        [format!(
            "{}: a file of project 1 under project 9",
            nine.display()
        )]
    );
    std::fs::remove_dir_all(&dir).unwrap();
}
