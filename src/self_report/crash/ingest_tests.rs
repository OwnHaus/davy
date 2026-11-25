use super::super::{Frame, dir, write};
use super::*;
use crate::{
    self_report::{Forwarder, forward},
    test_support::scratch,
};
use std::sync::Arc;

/// 2026-01-01T00:00:00Z.
const AT_MS: i64 = 1_767_225_600_000;
const ID: &str = "0123456789abcdef0123456789abcdef";

fn report(message: &str) -> Report {
    Report {
        format: FORMAT,
        event_id: ID.to_owned(),
        at_ms: AT_MS,
        release: "davy@1.2.3".to_owned(),
        thread: "log writer".to_owned(),
        message: message.to_owned(),
        location: Some("src/logs/store.rs:4:55".to_owned()),
        frames: vec![Frame {
            function: "davy::logs::store::State::add".to_owned(),
            file: Some("src/logs/store.rs".to_owned()),
            line: Some(4),
            column: Some(55),
        }],
    }
}

/// A store with the project `davy` and its writer.
struct Store {
    data: PathBuf,
    project: i64,
    writer: store::Writer,
    db: Mutex<Connection>,
    usage: usage::Counter,
    budget: Budget,
}

impl Store {
    fn new(name: &str) -> Self {
        let data = scratch(name);
        let mut c = store::open(&data).unwrap();
        let (project, _) = forward::find_or_create(&mut c).unwrap();
        let writer = store::Writer::start(c, 16);
        Self {
            db: Mutex::new(store::open(&data).unwrap()),
            data,
            project,
            writer,
            usage: usage::Counter::default(),
            budget: Budget::new(1 << 20),
        }
    }

    fn target<'a>(&'a self, policy: &'a Policy, project: i64) -> Target<'a> {
        Target {
            project,
            policy,
            sender: &self.writer.sender,
            budget: &self.budget,
            usage: &self.usage,
            db: &self.db,
        }
    }

    fn number(&self, sql: &str) -> i64 {
        let c = self.db.lock().unwrap();
        c.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    fn stop(self) {
        drop(self.writer.sender);
        drop(self.writer.ui);
        self.writer.thread.join().unwrap();
        drop(self.db);
        fs::remove_dir_all(&self.data).unwrap();
    }
}

#[test]
fn a_crash_file_becomes_an_issue_and_one_that_cannot_be_read_is_set_aside() {
    let s = Store::new("ingest-record");
    let dir = dir(&s.data);
    let good = write(&dir, &report("no value for ops@example.com")).unwrap();
    let bad = dir.join("1767225600001-bad.json");
    fs::write(&bad, b"{}").unwrap();
    let policy = forward::policy();
    let mut told = Told::default();
    let files = [good.clone(), bad.clone()];
    assert_eq!(record(&files, &s.target(&policy, s.project), &mut told), 1);
    assert!(!good.exists() && !bad.exists());
    assert!(dir.join(REFUSED).join("1767225600001-bad.json").exists());
    let c = s.db.lock().unwrap();
    let (title, level, release): (String, String, String) = c
        .query_row(
            "SELECT i.title, e.level, e.release FROM issues i JOIN events e ON e.issue_id = i.id WHERE i.project_id = ?1",
            [s.project],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    drop(c);
    assert_eq!(
        (title.as_str(), level.as_str(), release.as_str()),
        (
            "panic: no value for [Filtered: email]",
            "fatal",
            "davy@1.2.3"
        )
    );
    let rows = s.usage.take();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (rows[0].project, rows[0].signal, rows[0].count.records),
        (s.project, Signal::Errors, 1)
    );
    s.stop();
}

#[test]
fn a_crash_file_the_store_fails_to_take_waits_and_is_recorded_later() {
    let s = Store::new("ingest-failed");
    let path = write(&dir(&s.data), &report("no value")).unwrap();
    let policy = forward::policy();
    let mut told = Told::default();
    // No project has this id, so the store refuses the event.
    let missing = s.project + 1000;
    let files = [path.clone(), path.clone()];
    assert_eq!(record(&files, &s.target(&policy, missing), &mut told), 0);
    assert!(path.exists(), "a file the store failed to take is kept");
    assert_eq!(s.usage.take(), []);
    assert_eq!(s.number("SELECT count(*) FROM events"), 0);
    assert_eq!(
        record(&files[..1], &s.target(&policy, s.project), &mut told),
        1
    );
    assert!(!path.exists());
    assert_eq!(s.number("SELECT count(*) FROM events"), 1);
    s.stop();
}

#[test]
fn a_crash_file_waits_while_the_store_writer_is_gone() {
    let s = Store::new("ingest-stopped");
    let path = write(&dir(&s.data), &report("no value")).unwrap();
    let policy = forward::policy();
    let sender = crossbeam_channel::bounded(1).0;
    let target = Target {
        sender: &sender,
        ..s.target(&policy, s.project)
    };
    let mut told = Told::default();
    assert_eq!(record(std::slice::from_ref(&path), &target, &mut told), 0);
    assert!(path.exists());
    assert_eq!(s.usage.take(), []);
    s.stop();
}

#[test]
fn a_crash_file_recorded_before_is_deleted_and_not_counted_again() {
    let s = Store::new("ingest-known");
    let dir = dir(&s.data);
    let policy = forward::policy();
    let mut told = Told::default();
    let first = write(&dir, &report("no value")).unwrap();
    assert_eq!(
        record(&[first], &s.target(&policy, s.project), &mut told),
        1
    );
    s.usage.take();
    // The same file again, as when the delete after the store took it
    // failed.
    let again = write(&dir, &report("no value")).unwrap();
    assert_eq!(
        record(
            std::slice::from_ref(&again),
            &s.target(&policy, s.project),
            &mut told
        ),
        0
    );
    assert!(!again.exists());
    assert_eq!(s.usage.take(), []);
    assert_eq!(s.number("SELECT count(*) FROM events"), 1);
    s.stop();
}

#[test]
fn a_full_refused_directory_deletes_what_cannot_be_read() {
    let data = scratch("ingest-refused-full");
    let dir = dir(&data);
    let refused = dir.join(REFUSED);
    fs::create_dir_all(&refused).unwrap();
    for i in 0..MAX_PENDING {
        fs::write(refused.join(format!("{i:013}-x.json")), b"{}").unwrap();
    }
    let bad = dir.join("1767225600001-bad.json");
    fs::write(&bad, b"{}").unwrap();
    set_aside(&bad, "missing field `format`", &mut Told::default());
    assert!(!bad.exists());
    assert_eq!(pending(&refused).unwrap().len(), MAX_PENDING);
    fs::remove_dir_all(&data).unwrap();
}

#[test]
fn hidden_entries_and_other_names_are_not_listed() {
    let dir = scratch("ingest-list");
    fs::create_dir_all(&dir).unwrap();
    for name in [".1767225600000-a.json.tmp", ".hidden.json", "notes.txt"] {
        fs::write(dir.join(name), b"{}").unwrap();
    }
    fs::write(dir.join("1767225600000-a.json"), b"{}").unwrap();
    assert_eq!(
        list(&dir).unwrap(),
        Listing {
            files: vec![dir.join("1767225600000-a.json")],
            others: Vec::new(),
        }
    );
    assert_eq!(list(&dir.join("missing")).unwrap(), Listing::default());
    fs::remove_dir_all(&dir).unwrap();
}

#[cfg(unix)]
#[test]
fn a_fifo_a_link_and_a_directory_are_left_alone_without_blocking() {
    let dir = scratch("ingest-special");
    fs::create_dir_all(&dir).unwrap();
    let fifo = dir.join("1767225600000-fifo.json");
    nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::S_IRWXU).unwrap();
    let target = write(&dir.join("elsewhere"), &report("no value")).unwrap();
    let link = dir.join("1767225600001-link.json");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let directory = dir.join("1767225600002-dir.json");
    fs::create_dir(&directory).unwrap();
    assert_eq!(
        list(&dir).unwrap(),
        Listing {
            files: Vec::new(),
            others: vec![fifo.clone(), link.clone(), directory.clone()],
        }
    );
    // A FIFO without a writer would block a plain open forever.
    assert_eq!(
        format!("{:#}", read(&fifo).unwrap_err()),
        "not a regular file"
    );
    assert!(read(&link).is_err(), "a link is not followed");
    assert!(read(&directory).is_err());
    assert!(fifo.exists() && link.exists() && target.exists());
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_stale_hidden_file_is_swept_and_a_fresh_one_kept() {
    let dir = scratch("ingest-sweep");
    fs::create_dir_all(&dir).unwrap();
    let stale = dir.join(".1767225600000-a.json.tmp");
    let fresh = dir.join(".1767225600001-b.json.tmp");
    let file = fs::File::create(&stale).unwrap();
    let old = std::time::SystemTime::UNIX_EPOCH + Duration::from_hours(490_896);
    file.set_modified(old).unwrap();
    drop(file);
    fs::write(&fresh, b"").unwrap();
    sweep(&dir);
    assert!(!stale.exists());
    assert!(fresh.exists());
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_name_is_told_once_and_the_names_are_bounded() {
    let mut told = Told::default();
    assert!(told.first(Path::new("a")));
    assert!(!told.first(Path::new("a")));
    for i in 0..MAX_TOLD {
        told.first(Path::new(&i.to_string()));
    }
    assert!(!told.first(Path::new("one past the bound")));
}

#[test]
fn the_forwarder_records_waiting_crashes_when_it_starts() {
    let s = Store::new("ingest-forwarder");
    write(&dir(&s.data), &report("no value")).unwrap();
    let (logs, _inbox) = crate::logs::store::LogQueue::bounded(100, 1 << 20);
    let forwarder = Forwarder {
        data: s.data.clone(),
        logs,
        usage: usage::Counter::default(),
        policies: Arc::default(),
        sender: s.writer.sender.clone(),
        budget: Budget::new(1 << 20),
    };
    let (sink, lines) = crate::self_report::Sink::new(1);
    drop(sink);
    forwarder.start(lines).unwrap().join().unwrap();
    assert_eq!(pending(&dir(&s.data)).unwrap(), Vec::<PathBuf>::new());
    assert_eq!(
        s.number(&format!(
            "SELECT count(*) FROM issues WHERE project_id = {}",
            s.project
        )),
        1
    );
    s.stop();
}
