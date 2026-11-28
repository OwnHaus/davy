use super::*;
use crate::test_support::scratch;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::{fs, path::PathBuf};

fn root() -> PathBuf {
    let root = scratch("pause");
    fs::create_dir_all(&root).unwrap();
    root
}

fn enter(root: &Path) -> Result<Gate> {
    let mut gate = Gate::open(root)?;
    gate.check()?;
    Ok(gate)
}

/// A log writer on its own thread: each turn of its loop is a check and a
/// count. It keeps the last error of a check.
struct Writer {
    turns: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    error: Arc<Mutex<Option<String>>>,
    thread: thread::JoinHandle<()>,
}

impl Writer {
    fn start(root: &Path, patience: Duration) -> Self {
        let mut gate = enter(root).unwrap();
        gate.patience = patience;
        let turns = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let error = Arc::new(Mutex::new(None));
        let (t, s, e) = (turns.clone(), stop.clone(), error.clone());
        let thread = thread::spawn(move || {
            while !s.load(Ordering::SeqCst) {
                if let Err(err) = gate.check() {
                    *e.lock().unwrap() = Some(format!("{err:#}"));
                }
                t.fetch_add(1, Ordering::SeqCst);
                thread::yield_now();
            }
        });
        Self {
            turns,
            stop,
            error,
            thread,
        }
    }

    fn turns(&self) -> u64 {
        self.turns.load(Ordering::SeqCst)
    }

    /// Waits until the writer has turned past `n`.
    fn past(&self, n: u64) {
        while self.turns() <= n {
            thread::yield_now();
        }
    }

    fn stop(self) -> Option<String> {
        self.stop.store(true, Ordering::SeqCst);
        self.thread.join().unwrap();
        self.error.lock().unwrap().take()
    }
}

#[test]
fn without_a_writer_the_pause_starts_at_once_and_says_so() {
    let root = root();
    let paused = Paused::take(&root).unwrap();
    assert!(!paused.writer());
    paused.end().unwrap();
}

#[test]
fn the_writer_turns_no_more_during_the_pause_and_goes_on_after_it() {
    let root = root();
    let writer = Writer::start(&root, WRITER_WAIT);
    writer.past(3);
    let paused = Paused::take(&root).unwrap();
    assert!(paused.writer());
    let during = writer.turns();
    // A count of yields can take a minute on a loaded machine, longer than
    // the writer waits, so this spins for a time instead.
    let until = Instant::now() + Duration::from_millis(50);
    while Instant::now() < until {
        thread::yield_now();
    }
    assert_eq!(writer.turns(), during);
    paused.end().unwrap();
    writer.past(during + 2);
    assert_eq!(writer.stop(), None);
}

#[test]
fn a_backup_killed_during_its_pause_lets_the_writer_go_on() {
    let root = root();
    let writer = Writer::start(&root, WRITER_WAIT);
    let paused = Paused::take(&root).unwrap();
    let during = writer.turns();
    drop(paused);
    writer.past(during + 2);
    assert_eq!(writer.stop(), None);
    Paused::take(&root).unwrap().end().unwrap();
}

#[test]
fn a_writer_that_stops_waiting_makes_that_backup_fail() {
    let root = root();
    let writer = Writer::start(&root, Duration::from_millis(20));
    let paused = Paused::take(&root).unwrap();
    let during = writer.turns();
    writer.past(during + 2);
    let e = paused.end().unwrap_err();
    assert_eq!(
        format!("{e:#}"),
        "the log writer stopped waiting and wrote during the backup's pause: take the backup again"
    );
    // Once the pause is over it holds the log directory again and says so
    // in the count, so the next backup's pause holds.
    let count = root.join(GATE).join(LOOSE);
    while loose_count(&count).unwrap() != 2 {
        thread::yield_now();
    }
    assert_eq!(
        writer.stop().as_deref(),
        Some(
            "a backup held the log writer for 20 ms: it writes on until that backup ends, and that backup fails"
        )
    );
}

#[test]
fn a_backup_waits_for_a_writer_still_loose_from_the_last_one_to_hold_the_directory() {
    let root = root();
    let count = root.join(GATE).join(LOOSE);
    let writer = Writer::start(&root, Duration::from_millis(20));
    let first = Paused::take(&root).unwrap();
    while loose_count(&count).unwrap().is_multiple_of(2) {
        thread::yield_now();
    }
    let r = root.clone();
    let second = thread::spawn(move || {
        let p = Paused::take(&r).unwrap();
        assert!(p.writer());
        p.at
    });
    assert!(first.end().is_err());
    // It paused a writer that holds the directory, not one writing loose.
    assert!(second.join().unwrap().is_multiple_of(2));
    writer.stop();
}

#[test]
fn a_writer_that_died_loose_leaves_an_even_count_to_the_next() {
    let root = root();
    let count = root.join(GATE).join(LOOSE);
    fs::create_dir_all(root.join(GATE)).unwrap();
    fs::write(&count, "7").unwrap();
    let _gate = enter(&root).unwrap();
    assert_eq!(loose_count(&count).unwrap(), 8);
}

#[test]
fn a_writer_that_starts_during_a_pause_waits_for_its_end() {
    let root = root();
    let paused = Paused::take(&root).unwrap();
    let ended = Arc::new(AtomicBool::new(false));
    let seen = ended.clone();
    let r = root.clone();
    let starting = thread::spawn(move || {
        let gate = enter(&r).unwrap();
        assert!(seen.load(Ordering::SeqCst));
        gate
    });
    ended.store(true, Ordering::SeqCst);
    paused.end().unwrap();
    starting.join().unwrap();
}

#[test]
fn a_second_backup_waits_for_the_first() {
    let root = root();
    let first = Paused::take(&root).unwrap();
    let ended = Arc::new(AtomicBool::new(false));
    let seen = ended.clone();
    let r = root.clone();
    let second = thread::spawn(move || {
        let p = Paused::take(&r).unwrap();
        assert!(seen.load(Ordering::SeqCst));
        p.end().unwrap();
    });
    ended.store(true, Ordering::SeqCst);
    first.end().unwrap();
    second.join().unwrap();
}

#[cfg(unix)]
#[test]
fn the_gate_and_its_lock_files_are_for_the_owner_only() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = root();
    let _gate = enter(&root).unwrap();
    let mode = |p: &str| fs::metadata(root.join(p)).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(".backup"), 0o700);
    assert_eq!(mode(".backup/running"), 0o600);
    bump(&root.join(GATE).join(LOOSE)).unwrap();
    assert_eq!(mode(".backup/loose"), 0o600);
}

#[test]
fn a_writer_past_its_patience_writes_on_until_that_backup_ends() {
    let root = root();
    let count = root.join(GATE).join(LOOSE);
    let mut gate = enter(&root).unwrap();
    gate.patience = Duration::from_millis(100);
    let (paused_tx, paused_rx) = std::sync::mpsc::channel();
    let (end_tx, end_rx) = std::sync::mpsc::channel::<()>();
    let r = root.clone();
    let backup = thread::spawn(move || {
        let paused = Paused::take(&r).unwrap();
        paused_tx.send(()).unwrap();
        end_rx.recv().unwrap();
        paused.end()
    });
    // The writer lets go of the directory and waits its patience once.
    let mut failed = 0;
    while paused_rx.try_recv().is_err() {
        failed += u32::from(gate.check().is_err());
        thread::yield_now();
    }
    // A check that waited again would fail after its patience, as the
    // backup still holds the gate: these return Ok without waiting.
    for _ in 0..50 {
        failed += u32::from(gate.check().is_err());
    }
    assert_eq!(failed, 1);
    assert_eq!(loose_count(&count).unwrap(), 1);
    end_tx.send(()).unwrap();
    assert!(backup.join().unwrap().is_err());
    gate.check().unwrap();
    assert_eq!(loose_count(&count).unwrap(), 2);
}

/// A check that races the end of a pause returns Ok only while it holds
/// the log directory, so the next backup pauses it.
#[test]
fn a_check_that_races_the_end_of_a_pause_holds_the_directory() {
    let root = root();
    let until = Instant::now() + Duration::from_secs(2);
    let mut rounds = 0_u64;
    while Instant::now() < until {
        rounds += 1;
        let paused = Paused::take(&root).unwrap();
        let mut gate = Gate::open(&root).unwrap();
        let go = Arc::new(std::sync::Barrier::new(2));
        let g = go.clone();
        let spin = rounds % 4096;
        let t = thread::spawn(move || {
            g.wait();
            let r = gate.check().is_ok();
            (r, gate)
        });
        go.wait();
        for _ in 0..spin {
            std::hint::spin_loop();
        }
        paused.end().unwrap();
        let (ok, gate) = t.join().unwrap();
        assert!(
            !ok || gate.holding,
            "round {rounds}: the check returned Ok without the log directory"
        );
    }
}

/// A backup makes its staging directory, then locks it: a sweep between
/// the two must not take it. One a killed backup left goes once old.
#[test]
fn a_sweep_leaves_a_young_unlocked_staging_directory_and_takes_an_old_one() {
    let gate = root().join(GATE);
    super::super::link::mkdir_all(&gate).unwrap();
    let young = gate.join(format!("{NEW}1-1"));
    let old = gate.join(format!("{NEW}1-2"));
    let run = gate.join(format!("{RUN}1-3"));
    for dir in [&young, &old, &run] {
        fs::create_dir(dir).unwrap();
    }
    let at = std::time::SystemTime::now() - BACKUP_WAIT - Duration::from_secs(1);
    File::open(&old).unwrap().set_modified(at).unwrap();
    sweep(&gate);
    assert!(young.exists());
    assert!(!old.exists());
    assert!(!run.exists());
}
