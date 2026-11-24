use super::{Cache, Day, Stamp, cache, forget};
use crate::logs::store::{LogQuery, LogWriter, search};
use crate::logs::{LogLevel, LogRecord, LogSource};
use crate::test_support::scratch;
use crate::time::{day_of, now_ns};
use std::path::Path;

const DAY: i64 = 86_400_000_000_000;

fn record(timestamp: i64, message: &str) -> LogRecord {
    LogRecord {
        project_id: 1,
        timestamp,
        observed_at: timestamp,
        source: LogSource::Logtail,
        level: LogLevel::Info,
        message: message.into(),
        service: None,
        trace_id: None,
        span_id: None,
        attributes: serde_json::Map::new(),
    }
}

fn write(root: &Path, batch: Vec<LogRecord>) {
    let writer =
        LogWriter::start(root.to_path_buf(), 30, crate::logs::store::no_overrides()).unwrap();
    assert!(writer.queue.push(batch).is_ok());
    drop(writer.queue);
    writer.thread.join().unwrap();
}

fn messages(root: &Path, since: i64) -> anyhow::Result<Vec<String>> {
    let q = LogQuery {
        project: None,
        since,
        until: now_ns(),
        min_level: None,
        text: None,
        pattern: None,
        limit: 10,
        skip: 0,
    };
    Ok(search(root, &q)?.into_iter().map(|h| h.message).collect())
}

/// Noon UTC `n` days ago, so a test never straddles midnight.
fn noon(n: i64) -> i64 {
    let now = now_ns();
    now - now.rem_euclid(DAY) - n * DAY + DAY / 2
}

#[test]
fn a_search_sees_logs_committed_after_it_opened_the_day() {
    let root = scratch("days-commit");
    write(&root, vec![record(noon(2), "first")]);
    assert_eq!(messages(&root, noon(3)).unwrap(), ["first"]);
    write(&root, vec![record(noon(2) + 1, "second")]);
    assert_eq!(messages(&root, noon(3)).unwrap(), ["first", "second"]);
}

#[test]
fn an_unchanged_day_is_searched_from_the_index_already_open() {
    let root = scratch("days-reuse");
    write(&root, vec![record(noon(2), "kept")]);
    assert_eq!(messages(&root, noon(3)).unwrap(), ["kept"]);
    let dir = root.join(day_of(noon(2)).to_string());
    for f in std::fs::read_dir(&dir).unwrap() {
        let path = f.unwrap().path();
        if path.file_name().unwrap() != "meta.json" {
            std::fs::remove_file(path).unwrap();
        }
    }
    assert_eq!(messages(&root, noon(3)).unwrap(), ["kept"]);
    forget(&root, day_of(noon(2)));
    assert!(
        messages(&root, noon(3)).is_err(),
        "opened again without its files"
    );
}

#[test]
fn a_deleted_day_drops_out_of_search_and_of_the_cache() {
    let root = scratch("days-delete");
    write(&root, vec![record(noon(3), "old"), record(noon(1), "new")]);
    assert_eq!(messages(&root, noon(4)).unwrap(), ["old", "new"]);
    std::fs::remove_dir_all(root.join(day_of(noon(3)).to_string())).unwrap();
    assert_eq!(messages(&root, noon(4)).unwrap(), ["new"]);
    let open: Vec<_> = cache()
        .days
        .keys()
        .filter(|(r, _)| *r == root)
        .map(|(_, d)| *d)
        .collect();
    assert_eq!(open, [day_of(noon(1))]);
}

#[test]
fn the_cache_closes_the_least_recently_used_day_when_full() {
    let root = scratch("days-evict");
    write(&root, vec![record(noon(1), "one")]);
    let dir = root.join(day_of(noon(1)).to_string());
    let (day, stamp) = (Day::open(&dir).unwrap(), Stamp::of(&dir).unwrap());
    let key = |n: i64| (root.clone(), day_of(noon(n)));
    let mut c = Cache::default();
    c.insert(key(1), stamp, day.clone(), 2);
    c.insert(key(2), stamp, day.clone(), 2);
    assert!(c.get(&key(1), stamp).is_some());
    c.insert(key(3), stamp, day, 2);
    let mut open: Vec<_> = c.days.keys().map(|k| k.1).collect();
    open.sort();
    assert_eq!(open, [day_of(noon(3)), day_of(noon(1))]);
}
