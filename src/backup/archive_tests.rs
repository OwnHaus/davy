use super::*;

/// Keeps what it is handed, as a restore would.
#[derive(Default)]
struct Kept {
    header: Option<Header>,
    files: Vec<(PathBuf, Vec<u8>)>,
}

impl Sink for Kept {
    fn header(&mut self, header: &Header) -> Result<()> {
        self.header = Some(header.clone());
        Ok(())
    }
    fn file(&mut self, path: &Path, _: u64, data: &mut dyn Read) -> Result<()> {
        let mut bytes = Vec::new();
        data.read_to_end(&mut bytes)?;
        self.files.push((path.to_path_buf(), bytes));
        Ok(())
    }
}

fn header() -> Header {
    Header {
        format: FORMAT,
        davy_version: "0.2.0".into(),
        schema: 27,
        created_at: "2026-11-28T10:00:00Z".into(),
        files: 100,
        bytes: 1 << 20,
    }
}

fn backup(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut w = Writer::new(Vec::new(), &header()).unwrap();
    for (path, bytes) in files {
        w.add(Path::new(path), bytes.len() as u64, *bytes).unwrap();
    }
    w.finish().unwrap().0
}

/// A tar stream compressed with zstd, entries written as given, so a test
/// can make one that `Writer` never would.
fn raw(entries: &[(&str, tar::EntryType, &[u8])]) -> Vec<u8> {
    let mut tar = tar::Builder::new(Vec::new());
    for (name, kind, bytes) in entries {
        let mut h = tar::Header::new_gnu();
        h.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
        h.set_entry_type(*kind);
        h.set_size(bytes.len() as u64);
        h.set_mode(0o600);
        h.set_cksum();
        tar.append(&h, *bytes).unwrap();
    }
    zstd::encode_all(tar.into_inner().unwrap().as_slice(), 1).unwrap()
}

fn header_json() -> Vec<u8> {
    serde_json::to_vec(&header()).unwrap()
}

fn manifest_json(files: &[(&str, &[u8])]) -> Vec<u8> {
    let files: Vec<File> = files
        .iter()
        .map(|(path, bytes)| File {
            path: (*path).into(),
            size: bytes.len() as u64,
            blake3: blake3::hash(bytes).to_hex().to_string(),
        })
        .collect();
    let bytes = files.iter().map(|f| f.size).sum();
    serde_json::to_vec(&Manifest { files, bytes }).unwrap()
}

fn error(bytes: &[u8]) -> String {
    format!("{:#}", read(bytes, &mut Kept::default()).unwrap_err())
}

const REG: tar::EntryType = tar::EntryType::Regular;

#[test]
fn a_backup_reads_back_with_its_header_files_and_manifest() {
    let bytes = backup(&[("token", b"secret\n"), ("logs/2026-11-28/meta.json", b"{}")]);
    let mut kept = Kept::default();
    let (h, manifest) = read(bytes.as_slice(), &mut kept).unwrap();
    assert_eq!(h, header());
    assert_eq!(kept.header, Some(header()));
    assert_eq!(
        kept.files,
        vec![
            (PathBuf::from("token"), b"secret\n".to_vec()),
            (PathBuf::from("logs/2026-11-28/meta.json"), b"{}".to_vec()),
        ]
    );
    assert_eq!(manifest.bytes, 9);
    assert_eq!(manifest.files[0].path, "token");
    assert_eq!(
        manifest.files[0].blake3,
        blake3::hash(b"secret\n").to_hex().to_string()
    );
}

#[test]
fn standard_tar_lists_the_entries_with_owner_only_modes() {
    let bytes = backup(&[("token", b"x")]);
    let plain = zstd::decode_all(bytes.as_slice()).unwrap();
    let mut tar = tar::Archive::new(plain.as_slice());
    let listed: Vec<(String, u32)> = tar
        .entries()
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            let name = e.path().unwrap().display().to_string();
            (name, e.header().mode().unwrap())
        })
        .collect();
    assert_eq!(
        listed,
        vec![
            ("davy-backup.json".to_owned(), 0o600),
            ("data/token".to_owned(), 0o600),
            ("manifest.json".to_owned(), 0o600),
        ]
    );
}

#[test]
fn a_file_that_shrank_while_read_fails_the_add() {
    let mut w = Writer::new(Vec::new(), &header()).unwrap();
    let e = w.add(Path::new("token"), 10, &b"short"[..]).unwrap_err();
    assert_eq!(
        format!("{e:#}"),
        "token shrank from 10 to 5 bytes while it was read"
    );
}

#[test]
fn a_path_that_could_leave_the_data_directory_is_not_written() {
    let mut w = Writer::new(Vec::new(), &header()).unwrap();
    let e = w.add(Path::new("../token"), 1, &b"x"[..]).unwrap_err();
    assert_eq!(format!("{e:#}"), "../token is not a plain relative path");
}

#[test]
fn a_backup_cut_short_is_refused_at_every_length() {
    let bytes = backup(&[("token", b"secret\n"), ("setup.json", &[7; 3000])]);
    for len in [0, 10, bytes.len() / 2, bytes.len() - 1] {
        assert!(
            read(&bytes[..len], &mut Kept::default()).is_err(),
            "{len} of {} bytes read as whole",
            bytes.len()
        );
    }
}

#[test]
fn a_writer_dropped_before_finish_leaves_a_stream_that_is_refused() {
    let mut out = Vec::new();
    {
        let mut w = Writer::new(&mut out, &header()).unwrap();
        w.add(Path::new("token"), 1, &b"x"[..]).unwrap();
    }
    assert!(read(out.as_slice(), &mut Kept::default()).is_err());
}

#[test]
fn a_file_changed_after_the_manifest_was_written_is_refused() {
    let bytes = raw(&[
        (HEADER, REG, &header_json()),
        ("data/token", REG, b"evil"),
        (MANIFEST, REG, &manifest_json(&[("token", b"good")])),
    ]);
    assert_eq!(
        error(&bytes),
        format!(
            "token does not match the manifest: 4 bytes with BLAKE3 {}, not 4 bytes with {}",
            blake3::hash(b"evil").to_hex(),
            blake3::hash(b"good").to_hex()
        )
    );
}

#[test]
fn a_file_missing_from_the_backup_or_the_manifest_is_refused() {
    let missing = raw(&[
        (HEADER, REG, &header_json()),
        (MANIFEST, REG, &manifest_json(&[("token", b"x")])),
    ]);
    assert_eq!(
        error(&missing),
        "token is in the manifest but not in the backup"
    );
    let extra = raw(&[
        (HEADER, REG, &header_json()),
        ("data/token", REG, b"x"),
        (MANIFEST, REG, &manifest_json(&[])),
    ]);
    assert_eq!(
        error(&extra),
        "token is in the backup but not in the manifest"
    );
}

#[test]
fn paths_outside_the_data_directory_and_links_are_refused() {
    let cases: [(&str, tar::EntryType, &str); 4] = [
        (
            "data/../token",
            REG,
            "the backup holds data/../token, which is not a plain path",
        ),
        (
            "/data/token",
            REG,
            "the backup holds /data/token, outside data/",
        ),
        ("token", REG, "the backup holds token, outside data/"),
        (
            "data/token",
            tar::EntryType::Symlink,
            "the backup holds an entry that is not a file: data/token",
        ),
    ];
    for (name, kind, want) in cases {
        let bytes = raw(&[(HEADER, REG, &header_json()), (name, kind, b"")]);
        assert_eq!(error(&bytes), want, "{name}");
    }
}

#[test]
fn a_backup_must_start_with_its_header_and_end_with_its_manifest() {
    let headless = raw(&[("data/token", REG, b"x")]);
    assert_eq!(
        error(&headless),
        "not a Davy backup: it does not start with davy-backup.json"
    );
    let trailing = raw(&[
        (HEADER, REG, &header_json()),
        (MANIFEST, REG, &manifest_json(&[])),
        ("data/token", REG, b"x"),
    ]);
    assert_eq!(
        error(&trailing),
        "the backup holds an entry after its manifest"
    );
    let twice = raw(&[
        (HEADER, REG, &header_json()),
        ("data/token", REG, b"x"),
        ("data/token", REG, b"x"),
    ]);
    assert_eq!(error(&twice), "token is in the backup twice");
    assert_eq!(
        error(b"not zstd"),
        "not a Davy backup: Unknown frame descriptor"
    );
}

#[test]
fn a_backup_in_a_newer_format_is_refused_before_any_file() {
    let mut newer = header();
    newer.format = 2;
    newer.davy_version = "0.9.0".into();
    let bytes = raw(&[
        (HEADER, REG, &serde_json::to_vec(&newer).unwrap()),
        ("data/token", REG, b"x"),
    ]);
    let mut kept = Kept::default();
    let e = read(bytes.as_slice(), &mut kept).unwrap_err();
    assert_eq!(
        format!("{e:#}"),
        format!(
            "the backup is in format 2, which Davy {} does not read: restore it with the Davy 0.9.0 that took it, or a newer one",
            env!("CARGO_PKG_VERSION")
        )
    );
    assert_eq!(kept.header, None);
}

#[test]
fn the_sink_may_refuse_the_header_before_any_file() {
    struct Refuse(usize);
    impl Sink for Refuse {
        fn header(&mut self, _: &Header) -> Result<()> {
            bail!("too new")
        }
        fn file(&mut self, _: &Path, _: u64, _: &mut dyn Read) -> Result<()> {
            self.0 += 1;
            Ok(())
        }
    }
    let bytes = backup(&[("token", b"x")]);
    let mut sink = Refuse(0);
    let e = read(bytes.as_slice(), &mut sink).unwrap_err();
    assert_eq!(format!("{e:#}"), "too new");
    assert_eq!(sink.0, 0);
}

#[test]
fn the_writer_refuses_a_name_longer_than_a_tar_header_holds() {
    let longest = format!(
        "sourcemaps/{}",
        "a".repeat(MAX_NAME - "data/sourcemaps/".len())
    );
    let bytes = backup(&[(longest.as_str(), b"{}")]);
    let mut kept = Kept::default();
    read(bytes.as_slice(), &mut kept).unwrap();
    assert_eq!(kept.files, vec![(PathBuf::from(&longest), b"{}".to_vec())]);

    let long = format!("{longest}b");
    let mut w = Writer::new(Vec::new(), &header()).unwrap();
    let e = w.add(Path::new(&long), 2, &b"{}"[..]).unwrap_err();
    assert_eq!(
        format!("{e:#}"),
        format!("adding {long}: data/{long} is longer than the 100 bytes a backup holds in a name")
    );
}

/// The tar crate reads a GNU long name or a PAX header whole into memory
/// when it is not told to read raw entries; 8 MiB here took 288 MB once.
#[test]
fn a_tar_extension_entry_is_refused_before_it_is_read() {
    let big = vec![b'a'; 8 << 20];
    let cases: [(&str, tar::EntryType, &str); 3] = [
        ("pax", tar::EntryType::XHeader, "pax"),
        ("global", tar::EntryType::XGlobalHeader, "global"),
        (
            "././@LongLink",
            tar::EntryType::GNULongName,
            "././@LongLink",
        ),
    ];
    for (name, kind, shown) in cases {
        let first = raw(&[(name, kind, &big), (HEADER, REG, &header_json())]);
        assert_eq!(
            error(&first),
            format!("the backup holds an entry that is not a file: {shown}"),
            "{name} first"
        );
        let later = raw(&[
            (HEADER, REG, &header_json()),
            (name, kind, &big),
            ("data/token", REG, b"x"),
        ]);
        assert_eq!(
            error(&later),
            format!("the backup holds an entry that is not a file: {shown}"),
            "{name} later"
        );
    }
}

#[test]
fn names_that_start_with_a_dot_or_sit_beside_the_database_are_refused() {
    let cases = [
        (
            "data/.restore-ready/token",
            "data/.restore-ready/token, a name that starts with a dot",
        ),
        (
            "data/sourcemaps/.x.tmp-1",
            "data/sourcemaps/.x.tmp-1, a name that starts with a dot",
        ),
        (
            "data/davy.sqlite-wal",
            "data/davy.sqlite-wal, which no backup holds",
        ),
        (
            "data/davy.sqlite-shm",
            "data/davy.sqlite-shm, which no backup holds",
        ),
    ];
    for (name, want) in cases {
        let bytes = raw(&[(HEADER, REG, &header_json()), (name, REG, b"")]);
        let e = error(&bytes);
        assert!(e.starts_with(&format!("the backup holds {want}")), "{e}");
    }
    let nested = backup(&[("sourcemaps/davy.sqlite-wal", b"x")]);
    read(nested.as_slice(), &mut Kept::default()).unwrap();
}

#[test]
fn a_backup_past_the_files_or_the_bytes_its_header_gives_is_refused() {
    let mut small = header();
    small.files = 1;
    let json = serde_json::to_vec(&small).unwrap();
    let two = raw(&[
        (HEADER, REG, &json),
        ("data/a", REG, b"x"),
        ("data/b", REG, b"y"),
    ]);
    assert_eq!(
        error(&two),
        "the backup holds more than the 1 files of 1048576 bytes its header gives"
    );
    small.files = 10;
    small.bytes = 3;
    let json = serde_json::to_vec(&small).unwrap();
    let big = raw(&[
        (HEADER, REG, &json),
        ("data/a", REG, b"xy"),
        ("data/b", REG, b"yz"),
    ]);
    assert_eq!(
        error(&big),
        "the backup holds more than the 10 files of 3 bytes its header gives"
    );
}

#[test]
fn the_writer_refuses_a_file_past_the_files_or_the_bytes_of_its_header() {
    let mut small = header();
    small.files = 1;
    small.bytes = 4;
    let mut w = Writer::new(Vec::new(), &small).unwrap();
    let e = w.add(Path::new("a"), 5, &b"12345"[..]).unwrap_err();
    assert_eq!(
        format!("{e:#}"),
        "a makes the backup more than the 1 files of 4 bytes it listed: the data directory grew while it was written, take the backup again"
    );
    w.add(Path::new("a"), 4, &b"1234"[..]).unwrap();
    assert!(w.add(Path::new("b"), 0, &b""[..]).is_err());
}

#[test]
fn a_manifest_longer_than_its_files_can_take_is_refused() {
    let mut padded = manifest_json(&[("token", b"x")]);
    padded.extend(std::iter::repeat_n(b' ', 2000));
    let bytes = raw(&[
        (HEADER, REG, &header_json()),
        ("data/token", REG, b"x"),
        (MANIFEST, REG, &padded),
    ]);
    assert_eq!(error(&bytes), "manifest.json is over 1182 bytes");
}
