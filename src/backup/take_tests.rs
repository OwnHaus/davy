//! Backups of a directory that takes errors and logs while they run. See
//! "Backup and restore" in docs/design.md.

use super::*;
use crate::logs::{
    LogLevel, LogRecord, LogSource,
    store::{LogQuery, LogWriter, no_overrides, search_all},
};
use crate::test_support::scratch;
use std::{
    collections::BTreeSet,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
};

fn record(project_id: i64, at: i64, n: u64) -> LogRecord {
    LogRecord {
        project_id,
        timestamp: at,
        observed_at: at,
        source: LogSource::Logtail,
        level: LogLevel::Info,
        level_unknown: false,
        message: format!("order {n} paid"),
        service: None,
        trace_id: None,
        span_id: None,
        attributes: serde_json::Map::new(),
    }
}

/// The numbers of the logs in the log directory `root`.
fn logged(root: &Path) -> BTreeSet<u64> {
    let q = LogQuery {
        project: None,
        since: 0,
        until: i64::MAX,
        min_level: None,
        text: None,
        pattern: None,
        limit: 1_000_000,
        skip: 0,
    };
    search_all(root, &q)
        .unwrap()
        .iter()
        .map(|h| h.message.split(' ').nth(1).unwrap().parse().unwrap())
        .collect()
}

/// The numbers of the projects named `p<n>` in the data directory.
fn projects(data: &Path) -> BTreeSet<u64> {
    let c = crate::query::open(data).unwrap();
    let mut s = c
        .prepare("SELECT slug FROM projects WHERE slug LIKE 'p%'")
        .unwrap();
    s.query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|slug| slug.unwrap()[1..].parse().unwrap())
        .collect()
}

/// Whether `set` is `0..n` for some n: what the source held at one moment,
/// since each was written in order.
fn a_moment(set: &BTreeSet<u64>) -> bool {
    set.iter().copied().eq(0..set.len() as u64)
}

/// A source that takes logs and new projects, in order, on a thread of
/// its own, until it is stopped.
struct Ingest {
    stop: Arc<AtomicBool>,
    logs: Arc<AtomicU64>,
    thread: thread::JoinHandle<LogWriter>,
}

impl Ingest {
    fn start(data: &Path, writer: LogWriter, project: i64) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let logs = Arc::new(AtomicU64::new(0));
        let (s, l, data) = (stop.clone(), logs.clone(), data.to_owned());
        let thread = thread::spawn(move || {
            let c = store::open(&data).unwrap();
            let at = crate::time::now_ns();
            let mut n = 0;
            while !s.load(Ordering::SeqCst) {
                let batch = (n..n + 20).map(|i| record(project, at, i)).collect();
                // A full queue, as during a pause, takes the batch again.
                if writer.queue.push(batch).is_ok() {
                    store::create_project(&c, &format!("p{}", n / 20)).unwrap();
                    n += 20;
                    l.store(n, Ordering::SeqCst);
                }
                thread::yield_now();
            }
            writer
        });
        Self { stop, logs, thread }
    }

    /// Stops the source and its log writer, and returns how many logs it
    /// wrote.
    fn stop(self) -> u64 {
        self.stop.store(true, Ordering::SeqCst);
        let writer = self.thread.join().unwrap();
        drop(writer.queue);
        writer.thread.join().unwrap();
        self.logs.load(Ordering::SeqCst)
    }
}

/// A source with a project for its logs.
fn source() -> (PathBuf, i64) {
    let data = scratch("backup-src");
    let c = store::open(&data).unwrap();
    let (id, _) = store::create_project(&c, "shop").unwrap();
    (data, id)
}

/// The staging directories of backups in the log directory `root`.
fn staging(root: &Path) -> Vec<String> {
    link::entries(&root.join(pause::GATE))
        .unwrap()
        .into_iter()
        .map(|(name, ..)| name)
        .filter(|n| n.starts_with(pause::RUN) || n.starts_with(pause::NEW))
        .collect()
}

/// Waits until the log directory `root` holds `n` logs or more.
fn wait_for_logs(root: &Path, n: usize) {
    while logged(root).len() < n {
        thread::yield_now();
    }
}

#[test]
fn a_backup_during_ingest_restores_to_a_moment_of_the_source() {
    let (data, project) = source();
    let root = crate::logs::dir(&data);
    let writer = LogWriter::start(root.clone(), 30, no_overrides()).unwrap();
    let ingest = Ingest::start(&data, writer, project);
    wait_for_logs(&root, 100);
    let (first, taken) = take(&data, Vec::new()).unwrap();
    assert_eq!(taken.header.schema, store::SCHEMA_VERSION);
    assert!(taken.pause < Duration::from_secs(5), "{:?}", taken.pause);
    let written = ingest.logs.load(Ordering::SeqCst);
    wait_for_logs(&root, usize::try_from(written).unwrap() + 20);
    let second = take(&data, Vec::new()).unwrap().0;
    let total = ingest.stop();

    // The source keeps every log and project, and no staging directory.
    assert_eq!(logged(&root).len() as u64, total);
    assert_eq!(projects(&data).len() as u64, total / 20);
    assert_eq!(staging(&root), Vec::<String>::new());

    let mut seen = Vec::new();
    for backup in [first, second] {
        let into = scratch("backup-restored");
        super::super::unpack(&backup, &into);
        let logs = logged(&crate::logs::dir(&into));
        let made = projects(&into);
        assert!(a_moment(&logs), "{} logs, not 0..n", logs.len());
        assert!(a_moment(&made), "{} projects, not 0..n", made.len());
        assert!(!logs.is_empty() && logs.len() as u64 <= total);
        let c = store::open(&into).unwrap();
        let check: String = c
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .unwrap();
        assert_eq!(check, "ok");
        seen.push((into, logs.len()));
    }
    assert!(seen[0].1 < seen[1].1, "the second backup is later");

    // A server on the restored directory writes on where the backup ended.
    let (into, n) = &seen[1];
    let root = crate::logs::dir(into);
    let writer = LogWriter::start(root.clone(), 30, no_overrides()).unwrap();
    let more = (0..5).map(|i| record(project, crate::time::now_ns(), 1_000_000 + i));
    assert!(writer.queue.push(more.collect()).is_ok());
    drop(writer.queue);
    writer.thread.join().unwrap();
    assert_eq!(logged(&root).len(), n + 5);
}

/// Fails every write after `left` bytes, as a full disk or a closed pipe
/// would.
#[derive(Debug)]
struct Failing {
    left: usize,
}

impl Write for Failing {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.left == 0 {
            return Err(io::Error::other("no space left"));
        }
        let n = buf.len().min(self.left);
        self.left -= n;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_backup_that_fails_halfway_leaves_the_source_as_it_was() {
    let (data, project) = source();
    let root = crate::logs::dir(&data);
    let writer = LogWriter::start(root.clone(), 30, no_overrides()).unwrap();
    // 256 KiB that zstd cannot shrink, so the backup always passes the
    // 20,000 bytes the sink takes, however well the logs compress.
    let mut noise = vec![0_u8; 256 << 10];
    blake3::Hasher::new().finalize_xof().fill(&mut noise);
    fs::create_dir_all(data.join("a")).unwrap();
    fs::write(data.join("a/noise"), noise).unwrap();
    let ingest = Ingest::start(&data, writer, project);
    wait_for_logs(&root, 100);
    let e = take(&data, Failing { left: 20_000 }).unwrap_err();
    assert!(format!("{e:#}").contains("no space left"), "{e:#}");
    // The writer goes on, and the next backup pauses it again.
    let written = ingest.logs.load(Ordering::SeqCst);
    wait_for_logs(&root, usize::try_from(written).unwrap() + 20);
    let (_, taken) = take(&data, Vec::new()).unwrap();
    assert!(taken.files > 2);
    let total = ingest.stop();
    assert_eq!(logged(&root).len() as u64, total);
    assert_eq!(staging(&root), Vec::<String>::new());
}

#[test]
fn a_killed_backups_staging_directory_goes_with_the_next_backup() {
    let (data, _) = source();
    let gate = crate::logs::dir(&data).join(pause::GATE);
    let left = gate.join(format!("{}1-1", pause::RUN));
    fs::create_dir_all(left.join("logs")).unwrap();
    fs::write(left.join(DB_FILE), "half").unwrap();
    let (backup, _) = take(&data, Vec::new()).unwrap();
    assert!(!left.exists());
    let into = scratch("backup-restored");
    super::super::unpack(&backup, &into);
}

#[test]
fn a_backup_with_no_server_takes_the_directory_as_it_is() {
    let (data, _) = source();
    fs::create_dir_all(data.join("sourcemaps/shop")).unwrap();
    fs::write(data.join("sourcemaps/shop/app.js.map"), "{}").unwrap();
    // A source map an upload is writing, under its temporary name.
    fs::write(data.join("sourcemaps/shop/.app.js.map.tmp-1"), "{").unwrap();
    fs::write(data.join(RECORD), "{}").unwrap();
    let (backup, taken) = take(&data, Vec::new()).unwrap();
    assert_eq!(taken.pause.as_secs(), 0);
    let (_, manifest) = archive::read(backup.as_slice(), &mut archive::Check).unwrap();
    let paths: Vec<String> = manifest.files.iter().map(|f| f.path.clone()).collect();
    assert_eq!(paths, ["davy.sqlite", "sourcemaps/shop/app.js.map"]);
}

#[test]
fn a_database_an_older_server_holds_is_refused() {
    let (data, _) = source();
    let c = store::open(&data).unwrap();
    c.execute_batch("BEGIN; SELECT count(*) FROM sqlite_schema;")
        .unwrap();
    let e = take(&data, Vec::new()).unwrap_err();
    assert!(
        format!("{e:#}").contains("cannot pause for a backup"),
        "{e:#}"
    );
}

/// The slugs of the projects in a backup, restored.
fn restored_projects(backup: &[u8]) -> BTreeSet<u64> {
    let into = scratch("backup-restored");
    super::super::unpack(backup, &into);
    projects(&into)
}

/// Prefills a source with projects p0 to p2 and logs 0 to 59, committed,
/// and starts its log writer.
fn prefilled() -> (PathBuf, PathBuf, i64, LogWriter) {
    let (data, project) = source();
    let root = crate::logs::dir(&data);
    let writer = LogWriter::start(root.clone(), 30, no_overrides()).unwrap();
    let c = store::open(&data).unwrap();
    for p in 0..3 {
        store::create_project(&c, &format!("p{p}")).unwrap();
        let batch = (p * 20..p * 20 + 20).map(|i| record(project, crate::time::now_ns(), i));
        assert!(writer.queue.push(batch.collect()).is_ok());
    }
    wait_for_logs(&root, 60);
    (data, root, project, writer)
}

/// The rows of the database and the logs in a backup are of one moment.
/// Each step writes both; only what was written before the snapshot opened
/// inside the pause is in it. Without the pause the writer commits the logs
/// queued in it; with the snapshot opened before the pause or after it the
/// projects differ.
#[test]
fn a_backup_holds_the_rows_and_the_logs_of_one_moment() {
    let (data, root, project, writer) = prefilled();
    let c = store::open(&data).unwrap();
    let queue = writer.queue.clone();
    let push = |from: u64| {
        let batch = (from..from + 20).map(|i| record(project, crate::time::now_ns(), i));
        assert!(queue.push(batch.collect()).is_ok());
    };
    let (backup, _) = take_with(&data, Vec::new(), &mut |step| match step {
        Step::Paused => {
            store::create_project(&c, "p3").unwrap();
            push(60);
        }
        Step::InPause => {
            store::create_project(&c, "p4").unwrap();
            // A writer that is not paused commits the queued logs within
            // a second: for longer than that the source holds no more.
            let until = Instant::now() + Duration::from_millis(2500);
            while Instant::now() < until {
                assert_eq!(logged(&root).len(), 60, "the writer wrote in the pause");
            }
        }
        Step::AfterPause => {
            wait_for_logs(&root, 80);
            store::create_project(&c, "p5").unwrap();
            push(80);
        }
    })
    .unwrap();
    drop(queue);
    drop(writer.queue);
    writer.thread.join().unwrap();
    let into = scratch("backup-restored");
    super::super::unpack(&backup, &into);
    assert_eq!(projects(&into), BTreeSet::from([0, 1, 2, 3]));
    assert_eq!(logged(&crate::logs::dir(&into)), (0..60).collect());
}

/// The snapshot opens after the writer paused and before the logs are
/// linked, so a row written before the pause is in the backup and one
/// written during it is not.
#[test]
fn the_snapshot_opens_inside_the_pause() {
    let (data, _) = source();
    let writer = LogWriter::start(crate::logs::dir(&data), 30, no_overrides()).unwrap();
    let c = store::open(&data).unwrap();
    let (backup, _) = take_with(&data, Vec::new(), &mut |step| {
        let slug = match step {
            Step::Paused => "p0",
            Step::InPause => "p1",
            Step::AfterPause => "p2",
        };
        store::create_project(&c, slug).unwrap();
    })
    .unwrap();
    drop(writer.queue);
    writer.thread.join().unwrap();
    assert_eq!(restored_projects(&backup), BTreeSet::from([0]));
}

/// A crash report is linked before the snapshot opens: one the server
/// records into the database and deletes after the snapshot is still in
/// the backup, to be recorded again after a restore.
#[test]
fn a_crash_report_recorded_during_the_pause_is_in_the_backup() {
    let (data, _) = source();
    let writer = LogWriter::start(crate::logs::dir(&data), 30, no_overrides()).unwrap();
    let crashes = crash::dir(&data);
    fs::create_dir_all(&crashes).unwrap();
    fs::write(crashes.join("1.json"), "{}").unwrap();
    let (backup, _) = take_with(&data, Vec::new(), &mut |step| {
        if step == Step::InPause {
            fs::remove_file(crashes.join("1.json")).unwrap();
        }
    })
    .unwrap();
    drop(writer.queue);
    writer.thread.join().unwrap();
    let (_, manifest) = archive::read(backup.as_slice(), &mut archive::Check).unwrap();
    assert!(
        manifest
            .files
            .iter()
            .any(|f| f.path == format!("{}/1.json", crash::DIR)),
        "{:?}",
        manifest.files
    );
}

#[test]
fn the_snapshot_is_the_database_as_of_its_opening_not_of_its_copy() {
    let (data, _) = source();
    let c = store::open(&data).unwrap();
    let snapshot = Snapshot::open(&data).unwrap();
    store::create_project(&c, "late").unwrap();
    let to = scratch("snapshot");
    fs::create_dir_all(&to).unwrap();
    snapshot.copy(&to.join(DB_FILE)).unwrap();
    let copy = Connection::open(to.join(DB_FILE)).unwrap();
    let slugs: Vec<String> = copy
        .prepare("SELECT slug FROM projects ORDER BY slug")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(slugs, ["shop"]);
}

/// Removes the backup's staging directories once 1 MiB is written, as the
/// sweep of another process would one whose lock it could take.
#[derive(Debug)]
struct Swept {
    gate: PathBuf,
    done: bool,
    out: Vec<u8>,
}

impl Write for Swept {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // Past 1 MiB the backup is in the big file below: every file is
        // listed by then, and the staged log files are not yet read.
        if !self.done && self.out.len() >= 1 << 20 {
            self.done = true;
            for e in fs::read_dir(&self.gate).unwrap() {
                let e = e.unwrap();
                if e.file_name().to_string_lossy().starts_with(pause::RUN) {
                    fs::remove_dir_all(e.path()).unwrap();
                }
            }
        }
        self.out.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_backup_whose_staging_directory_is_removed_fails() {
    let (data, project) = source();
    let root = crate::logs::dir(&data);
    let writer = LogWriter::start(root.clone(), 30, no_overrides()).unwrap();
    let batch = (0..50).map(|i| record(project, crate::time::now_ns(), i));
    assert!(writer.queue.push(batch.collect()).is_ok());
    drop(writer.queue);
    writer.thread.join().unwrap();
    let mut noise = vec![0_u8; 4 << 20];
    blake3::Hasher::new().finalize_xof().fill(&mut noise);
    fs::create_dir_all(data.join("a")).unwrap();
    fs::write(data.join("a/noise"), noise).unwrap();
    let sink = Swept {
        gate: root.join(pause::GATE),
        done: false,
        out: Vec::new(),
    };
    let e = take(&data, sink).unwrap_err();
    assert!(
        format!("{e:#}").contains("was its staging directory removed?"),
        "{e:#}"
    );
}

/// A source with 50 logs written and its writer stopped.
fn logged_source() -> PathBuf {
    let (data, project) = source();
    let root = crate::logs::dir(&data);
    let writer = LogWriter::start(root, 30, no_overrides()).unwrap();
    let batch = (0..50).map(|i| record(project, crate::time::now_ns(), i));
    assert!(writer.queue.push(batch.collect()).is_ok());
    drop(writer.queue);
    writer.thread.join().unwrap();
    data
}

/// Runs `remove` on the staged log directory after the pause and returns
/// the error of the backup.
fn take_removing(data: &Path, remove: impl Fn(&Path)) -> String {
    let gate = crate::logs::dir(data).join(pause::GATE);
    let e = take_with(data, Vec::new(), &mut |step| {
        if step == Step::AfterPause {
            for (name, path, _) in link::entries(&gate).unwrap() {
                if name.starts_with(pause::RUN) {
                    remove(&path.join(crate::logs::DIR));
                }
            }
        }
    })
    .unwrap_err();
    format!("{e:#}")
}

/// `rm -rf` on a staging directory removes the links before it fails on
/// the directory, which holds the copy of the database by then.
#[test]
fn a_backup_whose_staged_logs_were_removed_fails() {
    let data = logged_source();
    let e = take_removing(&data, |logs| fs::remove_dir_all(logs).unwrap());
    assert!(e.starts_with("this backup linked "), "{e}");
    assert!(
        e.ends_with(" there: was its staging directory removed?"),
        "{e}"
    );
    assert!(e.contains(" and found 0 there"), "{e}");
}

#[test]
fn a_backup_that_lost_one_staged_day_fails() {
    let data = logged_source();
    let e = take_removing(&data, |logs| {
        let days: Vec<_> = link::entries(logs)
            .unwrap()
            .into_iter()
            .filter(|(.., kind)| kind.is_dir())
            .collect();
        assert_eq!(days.len(), 1);
        fs::remove_dir_all(&days[0].1).unwrap();
    });
    assert!(e.starts_with("this backup linked "), "{e}");
    assert!(
        e.ends_with(" there: was its staging directory removed?"),
        "{e}"
    );
}

/// A server that starts sweeps the gate, and so does every backup. One
/// between making a backup's staging directory and locking it removed it.
#[test]
fn a_sweep_during_a_backup_leaves_its_staging_directory() {
    let (data, _) = source();
    let gate = crate::logs::dir(&data).join(pause::GATE);
    let stop = Arc::new(AtomicBool::new(false));
    let sweeper = {
        let (gate, stop) = (gate.clone(), stop.clone());
        thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                pause::sweep(&gate);
            }
        })
    };
    let failed: Vec<String> = (0..200)
        .filter_map(|_| take(&data, Vec::new()).err())
        .map(|e| format!("{e:#}"))
        .collect();
    stop.store(true, Ordering::Relaxed);
    sweeper.join().unwrap();
    assert_eq!(failed, Vec::<String>::new());
}

/// A data directory that is an ext4 volume has a `lost+found` only root
/// reads, and a name longer than a tar header holds needs an extension
/// entry no backup holds. Davy writes neither.
#[test]
fn lost_and_found_and_a_name_too_long_are_left_out() {
    use std::os::unix::fs::PermissionsExt as _;
    let (data, _) = source();
    let lost = data.join("lost+found");
    fs::create_dir(&lost).unwrap();
    fs::write(lost.join("x"), "x").unwrap();
    fs::set_permissions(&lost, fs::Permissions::from_mode(0o000)).unwrap();
    let longest = format!("sourcemaps/{}", "a".repeat(100 - "data/sourcemaps/".len()));
    let long = format!("{longest}b");
    fs::create_dir(data.join("sourcemaps")).unwrap();
    fs::write(data.join(&longest), "{}").unwrap();
    fs::write(data.join(&long), "{}").unwrap();
    let taken = take(&data, Vec::new());
    fs::set_permissions(&lost, fs::Permissions::from_mode(0o700)).unwrap();
    let (backup, taken) = taken.unwrap();
    assert_eq!(taken.long, vec![long]);
    let (_, manifest) = archive::read(backup.as_slice(), &mut archive::Check).unwrap();
    let paths: Vec<String> = manifest.files.iter().map(|f| f.path.clone()).collect();
    assert_eq!(paths, ["davy.sqlite".to_owned(), longest]);
}
