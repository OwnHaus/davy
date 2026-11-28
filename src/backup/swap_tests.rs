use super::*;
use crate::test_support::scratch;

/// A data directory that held `notes` and `davy.sqlite`, and a restore of a
/// `token`, `sourcemaps/x` and a database stopped right after its commit.
fn committed() -> PathBuf {
    let data = scratch("swap");
    fs::create_dir_all(&data).unwrap();
    fs::write(data.join("notes"), "mine").unwrap();
    fs::write(data.join(DB_FILE), "old").unwrap();
    let ready = data.join(READY);
    fs::create_dir_all(ready.join("sourcemaps")).unwrap();
    fs::write(ready.join("token"), "secret").unwrap();
    fs::write(ready.join("sourcemaps/x"), "{}").unwrap();
    fs::write(ready.join(DB_FILE), "new").unwrap();
    fs::write(ready.join(ASIDE), ".replaced-T-1").unwrap();
    data
}

/// The names in `dir`, sorted, besides the claim, which stays.
fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n != CLAIM)
        .collect();
    names.sort();
    names
}

fn finished(data: &Path) {
    assert_eq!(
        names(data),
        vec![".replaced-T-1", DB_FILE, "sourcemaps", "token"]
    );
    assert_eq!(fs::read_to_string(data.join(DB_FILE)).unwrap(), "new");
    let aside = data.join(".replaced-T-1");
    assert_eq!(names(&aside), vec![DB_FILE, "notes"]);
    assert_eq!(fs::read_to_string(aside.join(DB_FILE)).unwrap(), "old");
}

#[test]
fn a_restore_stopped_right_after_its_commit_is_finished() {
    let data = committed();
    assert_eq!(finish(&data).unwrap(), Some(data.join(".replaced-T-1")));
    finished(&data);
    assert_eq!(finish(&data).unwrap(), None);
}

#[test]
fn a_restore_stopped_while_it_moved_the_old_files_aside_is_finished() {
    let data = committed();
    fs::create_dir(data.join(".replaced-T-1")).unwrap();
    fs::rename(data.join("notes"), data.join(".replaced-T-1/notes")).unwrap();
    finish(&data).unwrap();
    finished(&data);
}

#[test]
fn a_restore_stopped_while_it_moved_the_new_files_in_is_finished() {
    let data = committed();
    fs::create_dir(data.join(".replaced-T-1")).unwrap();
    for name in ["notes", DB_FILE] {
        fs::rename(data.join(name), data.join(".replaced-T-1").join(name)).unwrap();
    }
    fs::rename(data.join(READY), data.join(MOVING)).unwrap();
    fs::rename(data.join(MOVING).join("token"), data.join("token")).unwrap();
    finish(&data).unwrap();
    finished(&data);
}

#[test]
fn the_database_does_not_go_in_beside_files_sqlite_left_and_a_rerun_finishes() {
    let data = committed();
    fs::create_dir(data.join(".replaced-T-1")).unwrap();
    for name in ["notes", DB_FILE] {
        fs::rename(data.join(name), data.join(".replaced-T-1").join(name)).unwrap();
    }
    fs::rename(data.join(READY), data.join(MOVING)).unwrap();
    fs::write(data.join("davy.sqlite-wal"), "a process opened it").unwrap();
    let e = finish(&data).unwrap_err();
    assert_eq!(
        format!("{e:#}"),
        format!(
            "{} appeared during the restore: a process opened the database. Stop it and run davy restore again to finish",
            data.join("davy.sqlite-wal").display()
        )
    );
    assert!(data.join(MOVING).join(DB_FILE).exists());
    assert!(!data.join(DB_FILE).exists());
    fs::remove_file(data.join("davy.sqlite-wal")).unwrap();
    finish(&data).unwrap();
    finished(&data);
}

#[test]
fn a_server_that_starts_finishes_the_restore_first() {
    let data = committed();
    let _claim = Claim::serve(&data).unwrap();
    finished(&data);
}

#[test]
fn what_a_restore_stopped_before_its_commit_left_goes() {
    let data = scratch("swap");
    fs::create_dir_all(data.join(".restore-1-2/sourcemaps")).unwrap();
    fs::write(data.join("notes"), "mine").unwrap();
    assert_eq!(finish(&data).unwrap(), None);
    assert_eq!(names(&data), vec!["notes"]);
}

#[test]
fn an_aside_that_names_a_path_outside_is_refused() {
    let data = committed();
    fs::write(data.join(READY).join(ASIDE), "../elsewhere").unwrap();
    let e = finish(&data).unwrap_err();
    assert!(
        format!("{e:#}").ends_with("names \"../elsewhere\", not a directory of the data directory")
    );
    assert_eq!(fs::read_to_string(data.join("notes")).unwrap(), "mine");
}

#[test]
fn a_servers_claim_leaves_the_retention_lock_free() {
    let data = scratch("swap-claim");
    let _claim = Claim::serve(&data).unwrap();
    let lock = crate::retention::Lock::default()
        .with_dir(&data)
        .waiting(std::time::Duration::ZERO);
    lock.hold_all().unwrap();
    // And a restore's sweep leaves the claim alone.
    sweep(&data);
    assert!(data.join(CLAIM).is_dir());
}

/// A crash after the last entry moved in and `.aside` went, before the
/// empty journal did.
#[test]
fn a_restore_stopped_after_its_last_move_is_done() {
    let data = scratch("swap");
    fs::create_dir_all(data.join(MOVING)).unwrap();
    fs::write(data.join(DB_FILE), "new").unwrap();
    let _claim = Claim::serve(&data).unwrap();
    assert_eq!(names(&data), vec![DB_FILE]);
}

/// The restore moved the old database aside and stopped, then a davy
/// command made a new one in its place, before this fix. Finishing must
/// keep the old one, and a davy command refuses now.
#[test]
fn finishing_never_overwrites_what_was_moved_aside() {
    let data = committed();
    let aside = data.join(".replaced-T-1");
    fs::create_dir(&aside).unwrap();
    fs::rename(data.join(DB_FILE), aside.join(DB_FILE)).unwrap();
    let e = crate::store::open(&data).unwrap_err();
    let shown = data.display();
    assert_eq!(
        format!("{e:#}"),
        format!(
            "a restore into {shown} stopped halfway: start davy serve on it, or run davy restore --data {shown} -, which finishes it and reads nothing"
        )
    );
    fs::write(data.join(DB_FILE), "a command made it").unwrap();
    let e = finish(&data).unwrap_err();
    assert_eq!(
        format!("{e:#}"),
        format!(
            "davy.sqlite is in {shown} and in {}: a process wrote it after the restore stopped. Move one of them away and run davy restore again to finish",
            aside.display()
        )
    );
    assert_eq!(fs::read_to_string(aside.join(DB_FILE)).unwrap(), "old");
    assert_eq!(fs::read_to_string(data.join("notes")).unwrap(), "mine");
    fs::remove_file(data.join(DB_FILE)).unwrap();
    finish(&data).unwrap();
    finished(&data);
}

#[test]
fn a_file_in_the_way_of_a_restored_one_stops_the_move_until_it_goes() {
    let data = committed();
    fs::create_dir(data.join(".replaced-T-1")).unwrap();
    for name in ["notes", DB_FILE] {
        fs::rename(data.join(name), data.join(".replaced-T-1").join(name)).unwrap();
    }
    fs::rename(data.join(READY), data.join(MOVING)).unwrap();
    fs::write(data.join("token"), "a process wrote it").unwrap();
    let e = finish(&data).unwrap_err();
    assert_eq!(
        format!("{e:#}"),
        format!(
            "{} appeared during the restore: a process wrote it. Move it away and run davy restore again to finish",
            data.join("token").display()
        )
    );
    assert_eq!(
        fs::read_to_string(data.join(MOVING).join("token")).unwrap(),
        "secret"
    );
    fs::remove_file(data.join("token")).unwrap();
    finish(&data).unwrap();
    finished(&data);
}

/// Before this fix a file with the aside's name stopped every finish with
/// "Not a directory". Nothing was moved there, so another name does.
#[test]
fn a_file_with_the_name_of_the_aside_gets_the_old_files_another_name() {
    let data = committed();
    fs::write(data.join(".replaced-T-1"), "in the way").unwrap();
    let aside = finish(&data).unwrap().unwrap();
    let name = aside.file_name().unwrap().to_str().unwrap().to_owned();
    assert!(name.starts_with(".replaced-T-1-"), "{name}");
    assert_eq!(
        names(&data),
        vec![".replaced-T-1", &name, DB_FILE, "sourcemaps", "token"]
    );
    assert_eq!(names(&aside), vec![DB_FILE, "notes"]);
    assert_eq!(
        fs::read_to_string(data.join(".replaced-T-1")).unwrap(),
        "in the way"
    );
}

#[test]
fn a_davy_command_refuses_while_a_restore_runs() {
    let data = scratch("swap");
    drop(crate::store::open(&data).unwrap());
    let _restore = Claim::restore(&data).unwrap();
    for e in [
        crate::store::open(&data).map(drop).unwrap_err(),
        crate::query::open(&data).map(drop).unwrap_err(),
    ] {
        assert_eq!(
            format!("{e:#}"),
            format!(
                "a restore into {} runs: run this again when it ends",
                data.display()
            )
        );
    }
}

/// A server that cannot take the claim exclusive does not finish, so it
/// must not start on a journal another holder of the claim left.
#[test]
fn a_server_that_shares_the_claim_refuses_a_restore_stopped_halfway() {
    let data = committed();
    fs::create_dir(data.join(CLAIM)).unwrap();
    let held = File::open(data.join(CLAIM)).unwrap();
    held.lock_shared().unwrap();
    let e = Claim::serve(&data).map(drop).unwrap_err();
    assert!(
        format!("{e:#}").starts_with(&format!(
            "a restore into {} stopped halfway: ",
            data.display()
        )),
        "{e:#}"
    );
    assert_eq!(fs::read_to_string(data.join("notes")).unwrap(), "mine");
}

#[test]
fn the_aside_takes_a_name_nothing_in_the_data_directory_has() {
    let data = scratch("swap");
    fs::create_dir_all(data.join(".replaced-X")).unwrap();
    fs::write(data.join(".replaced-X-1"), "").unwrap();
    assert_eq!(free(&data, ".replaced-X").unwrap(), ".replaced-X-2");
    assert_eq!(free(&data, ".replaced-Y").unwrap(), ".replaced-Y");
    let staging = data.join(".restore-1-1");
    fs::create_dir(&staging).unwrap();
    seal(&data, &staging).unwrap();
    let name = fs::read_to_string(staging.join(ASIDE)).unwrap();
    assert!(name.starts_with(REPLACED), "{name}");
    assert!(!data.join(name).exists());
}
