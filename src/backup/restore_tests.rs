use super::*;
use crate::{backup::archive::Writer, test_support::scratch};

/// A data directory with a database, a secret and a source map.
fn source() -> PathBuf {
    let data = scratch("restore-src");
    drop(store::open(&data).unwrap());
    fs::write(data.join("token"), "secret").unwrap();
    fs::create_dir_all(data.join("sourcemaps/shop")).unwrap();
    fs::write(data.join("sourcemaps/shop/app.js.map"), "{}").unwrap();
    data
}

fn backup(data: &Path) -> Vec<u8> {
    crate::backup::take::take(data, Vec::new()).unwrap().0
}

/// The names in `dir`, sorted, besides the claim, which stays.
fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n != swap::CLAIM)
        .collect();
    names.sort();
    names
}

fn error(r: Result<Restored>) -> String {
    format!("{:#}", r.unwrap_err())
}

#[test]
fn a_backup_comes_back_whole_and_for_the_owner_only() {
    let bytes = backup(&source());
    let into = scratch("restore");
    let restored = restore(bytes.as_slice(), &into, false).unwrap();
    assert_eq!(restored.header.schema, SCHEMA_VERSION);
    assert_eq!(restored.replaced, None);
    assert_eq!(names(&into), vec!["davy.sqlite", "sourcemaps", "token"]);
    assert_eq!(fs::read_to_string(into.join("token")).unwrap(), "secret");
    let c = store::open(&into).unwrap();
    let version: u32 = c
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = |p: &str| fs::metadata(into.join(p)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode("token"), 0o600);
        assert_eq!(mode("sourcemaps/shop/app.js.map"), 0o600);
        assert_eq!(mode("sourcemaps/shop"), 0o700);
    }
}

#[test]
fn a_directory_that_holds_anything_is_refused() {
    let bytes = backup(&source());
    let into = scratch("restore");
    fs::create_dir_all(&into).unwrap();
    fs::write(into.join("notes"), "mine").unwrap();
    assert_eq!(
        error(restore(bytes.as_slice(), &into, false)),
        format!(
            "{0} is not empty: restore into a new directory, or pass --replace to move what it holds into {0}/.replaced-TIME-PID first",
            into.display()
        )
    );
    assert_eq!(names(&into), vec!["notes"]);
}

#[test]
fn replace_moves_what_the_directory_held_aside() {
    let bytes = backup(&source());
    let into = scratch("restore");
    drop(store::open(&into).unwrap());
    fs::write(into.join("token"), "old").unwrap();
    let restored = restore(bytes.as_slice(), &into, true).unwrap();
    let aside = restored.replaced.unwrap();
    assert!(aside.starts_with(&into));
    assert_eq!(fs::read_to_string(aside.join("token")).unwrap(), "old");
    assert!(aside.join("davy.sqlite").exists());
    assert_eq!(fs::read_to_string(into.join("token")).unwrap(), "secret");
    // What a replace moved aside does not count as holding anything.
    fs::remove_dir_all(into.join("sourcemaps")).unwrap();
    fs::remove_file(into.join("token")).unwrap();
    fs::remove_file(into.join("davy.sqlite")).unwrap();
    restore(bytes.as_slice(), &into, false).unwrap();
}

#[test]
fn a_directory_a_server_runs_on_is_refused_even_with_replace() {
    let bytes = backup(&source());
    let into = scratch("restore");
    let _writer = pause::Gate::open(&crate::logs::dir(&into)).unwrap();
    assert_eq!(
        error(restore(bytes.as_slice(), &into, true)),
        format!(
            "a Davy server runs on {}: stop it before you restore into it",
            into.display()
        )
    );
}

#[test]
fn a_database_another_process_has_open_is_refused() {
    let bytes = backup(&source());
    let into = scratch("restore");
    let c = store::open(&into).unwrap();
    c.execute_batch("BEGIN; SELECT count(*) FROM sqlite_schema;")
        .unwrap();
    assert_eq!(
        error(restore(bytes.as_slice(), &into, true)),
        format!(
            "another process has {} open: stop it before you restore into {}",
            into.join(DB_FILE).display(),
            into.display()
        )
    );
}

#[test]
fn a_backup_from_a_newer_schema_is_refused_before_any_file() {
    let header = Header::now(SCHEMA_VERSION + 1, 0, 10, 1 << 20);
    let mut w = Writer::new(Vec::new(), &header).unwrap();
    w.add(Path::new("token"), 6, &b"secret"[..]).unwrap();
    let bytes = w.finish().unwrap().0;
    let into = scratch("restore");
    assert_eq!(
        error(restore(bytes.as_slice(), &into, false)),
        format!(
            "the backup is from Davy {0}, at schema version {1}, and this Davy {0} knows versions up to {SCHEMA_VERSION}: restore it with Davy {0} or newer",
            env!("CARGO_PKG_VERSION"),
            SCHEMA_VERSION + 1
        )
    );
    assert_eq!(names(&into), Vec::<String>::new());
}

#[test]
fn a_damaged_backup_puts_nothing_in_place_and_moves_nothing_aside() {
    let mut bytes = backup(&source());
    bytes.truncate(bytes.len() - 40);
    let into = scratch("restore");
    fs::create_dir_all(&into).unwrap();
    fs::write(into.join("token"), "old").unwrap();
    assert!(restore(bytes.as_slice(), &into, true).is_err());
    assert_eq!(names(&into), vec!["token"]);
    assert_eq!(fs::read_to_string(into.join("token")).unwrap(), "old");
}

#[test]
fn a_backup_without_a_database_is_refused() {
    let header = Header::now(SCHEMA_VERSION, 0, 10, 1 << 20);
    let w = Writer::new(Vec::new(), &header).unwrap();
    let bytes = w.finish().unwrap().0;
    let into = scratch("restore");
    assert_eq!(
        error(restore(bytes.as_slice(), &into, false)),
        "the backup holds no davy.sqlite"
    );
    assert_eq!(names(&into), Vec::<String>::new());
}

#[test]
fn a_backup_from_an_older_schema_is_restored_as_it_is_and_upgraded_at_the_next_open() {
    let data = scratch("restore-src");
    let behind = store::one_behind(&data);
    let bytes = backup(&data);
    let into = scratch("restore");
    let restored = restore(bytes.as_slice(), &into, false).unwrap();
    assert_eq!(restored.header.schema, behind);
    assert_eq!(user_version(&into.join(DB_FILE)).unwrap(), behind);
    let c = store::open(&into).unwrap();
    let version: u32 = c
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
}

/// Hands out the backup, and starts a log writer and writes a database on
/// `into` before its first byte, as a server older than this Davy would,
/// without the claim and through no check of this Davy: after restore's
/// first checks, before its files move into place.
struct ServerStarts<'a> {
    bytes: &'a [u8],
    into: PathBuf,
    server: Option<(rusqlite::Connection, crate::logs::store::LogWriter)>,
}

impl Read for ServerStarts<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.server.is_none() {
            let c = rusqlite::Connection::open(self.into.join(DB_FILE)).unwrap();
            c.execute_batch(
                "CREATE TABLE projects (slug TEXT); INSERT INTO projects VALUES ('live');",
            )
            .unwrap();
            let w = crate::logs::store::LogWriter::start(
                crate::logs::dir(&self.into),
                30,
                crate::logs::store::no_overrides(),
            )
            .unwrap();
            self.server = Some((c, w));
        }
        self.bytes.read(buf)
    }
}

#[test]
fn a_server_that_starts_during_a_restore_is_refused_before_anything_moves() {
    for with_logs in [false, true] {
        let src = source();
        if with_logs {
            fs::create_dir_all(src.join("logs/cold/2026-11-01/1")).unwrap();
            fs::write(src.join("logs/cold/2026-11-01/1/shop.dlc"), "cold").unwrap();
        }
        let bytes = backup(&src);
        let into = scratch("restore");
        let mut input = ServerStarts {
            bytes: &bytes,
            into: into.clone(),
            server: None,
        };
        assert_eq!(
            error(restore(&mut input, &into, true)),
            format!(
                "a Davy server runs on {}: stop it before you restore into it",
                into.display()
            ),
            "with logs: {with_logs}"
        );
        let (server, _writer) = input.server.as_ref().unwrap();
        let slug: String = server
            .query_row("SELECT slug FROM projects", [], |r| r.get(0))
            .unwrap();
        assert_eq!(slug, "live");
        assert!(!into.join("token").exists());
    }
}

#[test]
fn a_restore_waits_for_no_server_of_this_davy_and_no_server_for_it() {
    let bytes = backup(&source());
    let into = scratch("restore");
    let claim = Claim::serve(&into).unwrap();
    assert_eq!(
        error(restore(bytes.as_slice(), &into, false)),
        format!(
            "a Davy server or another restore runs on {}, or a backup of it: stop it before you restore into it",
            into.display()
        )
    );
    drop(claim);
    let restoring = Claim::restore(&into).unwrap();
    assert_eq!(
        format!("{:#}", Claim::serve(&into).err().unwrap()),
        format!(
            "a restore into {} runs: start the server when it ends",
            into.display()
        )
    );
    drop(restoring);
    restore(bytes.as_slice(), &into, false).unwrap();
}

#[test]
fn a_name_that_starts_with_a_dot_is_refused_before_anything_moves() {
    let src = source();
    let db = fs::read(src.join(DB_FILE)).unwrap();
    let header = Header::now(SCHEMA_VERSION, 0, 10, 1 << 20);
    let mut w = Writer::new(Vec::new(), &header).unwrap();
    w.add(Path::new(DB_FILE), db.len() as u64, db.as_slice())
        .unwrap();
    w.add(Path::new(".x"), 1, &b"x"[..]).unwrap();
    let bytes = w.finish().unwrap().0;
    let into = scratch("restore");
    fs::create_dir_all(&into).unwrap();
    fs::write(into.join("notes"), "mine").unwrap();
    assert_eq!(
        error(restore(bytes.as_slice(), &into, true)),
        "the backup holds data/.x, a name that starts with a dot, which no backup holds"
    );
    assert_eq!(names(&into), vec!["notes"]);
}

#[test]
fn a_database_at_another_schema_than_its_header_says_is_refused() {
    let src = source();
    let db = fs::read(src.join(DB_FILE)).unwrap();
    let header = Header::now(SCHEMA_VERSION - 1, 0, 10, 1 << 20);
    let mut w = Writer::new(Vec::new(), &header).unwrap();
    w.add(Path::new(DB_FILE), db.len() as u64, db.as_slice())
        .unwrap();
    let bytes = w.finish().unwrap().0;
    let into = scratch("restore");
    assert_eq!(
        error(restore(bytes.as_slice(), &into, false)),
        format!(
            "the backup's database is at schema version {SCHEMA_VERSION}, its header says {}",
            SCHEMA_VERSION - 1
        )
    );
    assert_eq!(names(&into), Vec::<String>::new());
}

#[test]
fn a_backup_bigger_than_the_free_space_is_refused_before_any_file() {
    let header = Header::now(SCHEMA_VERSION, 0, 10, u64::MAX / 2);
    let mut w = Writer::new(Vec::new(), &header).unwrap();
    w.add(Path::new("token"), 6, &b"secret"[..]).unwrap();
    let bytes = w.finish().unwrap().0;
    let into = scratch("restore");
    let e = error(restore(bytes.as_slice(), &into, false));
    assert!(
        e.starts_with(&format!(
            "the backup holds 9223 PB and {} has ",
            into.display()
        )),
        "{e}"
    );
    assert!(e.ends_with("free: make room and restore again"), "{e}");
    assert_eq!(names(&into), Vec::<String>::new());
}

#[test]
fn a_directory_of_another_user_is_refused_before_a_claim_is_made_in_it() {
    let me = nix::unistd::geteuid().as_raw();
    if me == 0 {
        return;
    }
    let root = Path::new("/");
    let refused = format!(
        "/ belongs to uid 0 and this runs as uid {me}: run davy restore as the user that runs the server, such as sudo runuser -u davy -- davy restore ..."
    );
    assert_eq!(error(restore(&b""[..], root, true)), refused);
    assert_eq!(format!("{:#}", finish(root).unwrap_err()), refused);
    assert!(!root.join(swap::CLAIM).exists());
}
