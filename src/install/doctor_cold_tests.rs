use super::*;
use crate::logs::{LogLevel, LogRecord, LogSource, store::cold::cold_day};
use crate::test_support::scratch;
use crate::time::{DAY_NS, day_of, midnight, now_ns};

const D: &str = "/var/lib/davy";

fn record(project_id: i64, timestamp: i64) -> LogRecord {
    LogRecord {
        project_id,
        timestamp,
        observed_at: now_ns(),
        source: LogSource::Otlp,
        level: LogLevel::Info,
        level_unknown: false,
        message: "job done".to_owned(),
        service: None,
        trace_id: None,
        span_id: None,
        attributes: serde_json::Map::new(),
    }
}

#[test]
fn the_check_sizes_both_tiers_and_warns_on_a_block_that_fails() {
    let data = scratch("doctor-cold");
    let root = crate::logs::dir(&data);
    std::fs::create_dir_all(&root).unwrap();
    let today = day_of(now_ns());
    assert_eq!(
        cold(&data, D, today),
        ok("cold", "hot logs take 0 B, no day is cold yet")
    );

    let day = day_of(now_ns() - 10 * DAY_NS);
    let logs = (0..20).map(|i| record(1 + i % 2, midnight(day) + i));
    cold_day(&root, logs.collect()).unwrap();
    let seg = root.join("cold").join(day.to_string()).join("1");
    let len = |p: &str| std::fs::metadata(seg.join(p)).unwrap().len();
    let taken = size(len("1.dlc") + len("2.dlc"));
    assert_eq!(
        cold(&data, D, today),
        ok(
            "cold",
            format!(
                "hot logs take 0 B, cold logs take {taken} in 1 day, 2 blocks in 2 files, every block's checksum matches"
            )
        )
    );

    std::fs::write(seg.join("2.dlc"), b"not a cold file at all").unwrap();
    let file = seg.join("2.dlc");
    assert_eq!(
        cold(&data, D, today),
        not_ok(
            "cold",
            Status::Warn,
            format!(
                "hot logs take 0 B, cold logs take {} in 1 day, 1 block in 2 files, 1 file or block failed the check, which searches skip: {}: a cold file of 22 bytes",
                size(len("1.dlc") + 22),
                file.display()
            ),
            "restore each file named from a backup, or delete it to drop its logs; the rest of its day is still searched",
        )
    );
    std::fs::remove_dir_all(&data).unwrap();
}
