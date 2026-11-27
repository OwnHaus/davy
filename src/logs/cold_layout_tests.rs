use super::{
    days, files, project_of, remove_seal, seal, segment_dir, segments, temp_dir, temp_of, temps,
    write_seal,
};
use crate::test_support::scratch;
use jiff::civil::date;

#[test]
fn names_are_read_one_way_only() {
    assert_eq!(project_of("42.dlc"), Some(42));
    assert_eq!(project_of("042.dlc"), None);
    assert_eq!(project_of("+42.dlc"), None);
    assert_eq!(project_of("42.dlc.tmp"), None);
    assert_eq!(project_of(".dlc"), None);
    assert_eq!(temp_of(".3.tmp"), Some(3));
    assert_eq!(temp_of(".0.tmp"), None);
    assert_eq!(temp_of(".03.tmp"), None);
    assert_eq!(temp_of("3.tmp"), None);
}

#[test]
fn days_segments_and_files_are_listed_without_strays() {
    let root = scratch("cold-layout");
    let day = date(2026, 11, 20);
    for seq in [1, 2, 10] {
        std::fs::create_dir_all(segment_dir(&root, day, seq)).unwrap();
    }
    std::fs::create_dir_all(temp_dir(&root, day, 11)).unwrap();
    let cold = root.join("cold");
    std::fs::create_dir_all(cold.join("2026-11-21/01")).unwrap();
    std::fs::create_dir_all(cold.join("+002026-11-22")).unwrap();
    std::fs::write(cold.join("2026-11-23"), b"a file").unwrap();
    std::os::unix::fs::symlink(&cold, cold.join("2026-11-24")).unwrap();
    let seg = segment_dir(&root, day, 1);
    std::os::unix::fs::symlink(seg.join("7.dlc"), seg.join("8.dlc")).unwrap();
    std::fs::write(seg.join("7.dlc"), b"x").unwrap();
    std::fs::write(seg.join("patterns-7.json"), b"{}").unwrap();
    std::fs::create_dir_all(seg.join("9.dlc")).unwrap();

    assert_eq!(days(&root).unwrap(), vec![day, date(2026, 11, 21)]);
    assert_eq!(segments(&root, day).unwrap(), vec![1, 2, 10]);
    assert_eq!(
        segments(&root, date(2026, 11, 21)).unwrap(),
        Vec::<u32>::new()
    );
    assert_eq!(temps(&root, day).unwrap(), vec![11]);
    assert_eq!(files(&seg).unwrap(), vec![(7, seg.join("7.dlc"))]);
    assert!(segments(&root, date(2026, 11, 24)).is_err());
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn a_seal_is_written_read_and_removed() {
    let hot = scratch("cold-seal");
    std::fs::create_dir_all(&hot).unwrap();
    assert_eq!(seal(&hot).unwrap(), None);
    write_seal(&hot, 3).unwrap();
    assert_eq!(seal(&hot).unwrap(), Some(3));
    write_seal(&hot, 4).unwrap();
    assert_eq!(seal(&hot).unwrap(), Some(4));
    std::fs::write(hot.join("cold-seal"), b"4x").unwrap();
    assert_eq!(seal(&hot).unwrap(), None);
    remove_seal(&hot).unwrap();
    remove_seal(&hot).unwrap();
    assert_eq!(seal(&hot).unwrap(), None);
    assert_eq!(seal(&hot.join("missing")).unwrap(), None);
    std::fs::remove_dir_all(&hot).unwrap();
}
