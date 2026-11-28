use super::*;
use crate::test_support::scratch;

const KEPT: &str = "0f6e1c4a2b3d4e5f60718293a4b5c6d7";
const MERGING: &str = "aaaabbbbccccddddeeeeffff00001111";

fn write(path: &Path, bytes: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

/// Every file under `dir`, dot names included, relative and sorted.
fn listed(dir: &Path) -> Vec<String> {
    fn walk(dir: &Path, rel: &Path, out: &mut Vec<String>) {
        for e in fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            let rel = rel.join(e.file_name());
            if e.file_type().unwrap().is_dir() {
                walk(&e.path(), &rel, out);
            } else {
                out.push(rel.display().to_string());
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, Path::new(""), &mut out);
    out.sort();
    out
}

/// Where a backup links the log directory `root`, its parent made.
fn staging(root: &Path) -> PathBuf {
    fs::create_dir_all(root.join(".backup")).unwrap();
    root.join(".backup/run")
}

fn meta() -> String {
    meta_of(KEPT)
}

/// A `meta.json` that names the one segment `seg`.
fn meta_of(seg: &str) -> String {
    let id = format!(
        "{}-{}-{}-{}-{}",
        &seg[..8],
        &seg[8..12],
        &seg[12..16],
        &seg[16..20],
        &seg[20..]
    );
    format!(r#"{{"segments":[{{"segment_id":"{id}","max_doc":3,"deletes":null}}],"opstamp":4}}"#)
}

#[test]
fn a_day_links_the_segments_its_meta_names_and_what_davy_wrote_beside_them() {
    let root = scratch("link");
    let day = root.join("2026-11-27");
    write(&day.join("meta.json"), &meta());
    write(&day.join(".managed.json"), "[]");
    write(&day.join(format!("{KEPT}.idx")), "kept");
    write(&day.join(format!("{KEPT}.store")), "kept");
    write(&day.join(format!("{MERGING}.idx")), "half");
    write(&day.join(".tantivy-writer.lock"), "");
    write(&day.join("patterns-shop.json"), "{}");
    write(&day.join("cold-seal"), "2");
    write(&root.join("cold/2026-11-01/1/shop.dlc"), "cold");
    write(&root.join("cold/2026-11-01/.2.tmp/shop.dlc"), "half");
    write(&root.join(".trash/2026-10-01/x"), "gone");
    write(&root.join(".backup/running"), "");
    let into = staging(&root);
    let linked = logs(&root, &into).unwrap();
    assert_eq!(
        listed(&into),
        vec![
            &format!("2026-11-27/{KEPT}.idx"),
            &format!("2026-11-27/{KEPT}.store"),
            "2026-11-27/cold-seal",
            "2026-11-27/meta.json",
            "2026-11-27/patterns-shop.json",
            "cold/2026-11-01/1/shop.dlc",
        ]
    );
    assert_eq!(
        linked,
        Linked {
            files: 6,
            skipped: vec![]
        }
    );
    assert_eq!(
        fs::read_to_string(into.join("2026-11-27/meta.json")).unwrap(),
        meta()
    );
}

#[cfg(unix)]
#[test]
fn a_link_shares_the_file_so_a_later_delete_keeps_its_bytes() {
    use std::os::unix::fs::MetadataExt as _;
    let root = scratch("link");
    let day = root.join("2026-11-27");
    write(&day.join("meta.json"), &meta());
    write(&day.join(format!("{KEPT}.idx")), "kept");
    let into = staging(&root);
    logs(&root, &into).unwrap();
    let inode = |p: &Path| fs::metadata(p).unwrap().ino();
    let name = format!("2026-11-27/{KEPT}.idx");
    assert_eq!(inode(&root.join(&name)), inode(&into.join(&name)));
    fs::remove_dir_all(&day).unwrap();
    assert_eq!(fs::read_to_string(into.join(&name)).unwrap(), "kept");
}

#[cfg(unix)]
#[test]
fn a_symbolic_link_is_left_out_and_named() {
    let root = scratch("link");
    write(&root.join("elsewhere/x"), "x");
    std::os::unix::fs::symlink(root.join("elsewhere"), root.join("cold")).unwrap();
    let into = staging(&root);
    let linked = logs(&root, &into).unwrap();
    assert_eq!(linked.skipped, vec!["cold".to_owned()]);
    assert_eq!(linked.files, 1);
}

#[test]
fn a_meta_that_names_no_segments_is_an_error() {
    let root = scratch("link");
    write(&root.join("2026-11-27/meta.json"), "{}");
    let e = logs(&root, &staging(&root)).unwrap_err();
    assert_eq!(
        format!("{e:#}"),
        format!(
            "reading {}: no list of segments",
            root.join("2026-11-27/meta.json").display()
        )
    );
}

#[test]
fn segment_files_are_told_apart_by_their_name() {
    assert_eq!(segment_of(&format!("{KEPT}.idx")), Some(KEPT));
    assert_eq!(segment_of(&format!("{KEPT}.4.del")), Some(KEPT));
    assert_eq!(segment_of("patterns-shop.json"), None);
    assert_eq!(segment_of("meta.json"), None);
}

#[test]
fn a_meta_that_a_merge_replaces_while_its_day_is_linked_is_linked_again() {
    let root = scratch("link");
    let dir = root.join("2026-11-27");
    write(&dir.join("meta.json"), &meta());
    write(&dir.join(format!("{KEPT}.idx")), "kept");
    let into = staging(&root);
    mkdir(&into).unwrap();
    let mut rounds = 0;
    let mut merge = || {
        rounds += 1;
        if rounds == 1 {
            write(&dir.join(format!("{MERGING}.idx")), "merged");
            write(&dir.join("meta.json"), &meta_of(MERGING));
            fs::remove_file(dir.join(format!("{KEPT}.idx"))).unwrap();
        }
    };
    let mut linked = Linked::default();
    day(&dir, &into.join("2026-11-27"), &mut linked, &mut merge).unwrap();
    assert_eq!(rounds, 2);
    assert_eq!(
        listed(&into),
        vec![
            format!("2026-11-27/{MERGING}.idx"),
            "2026-11-27/meta.json".to_owned()
        ]
    );
    assert_eq!(
        fs::read_to_string(into.join("2026-11-27/meta.json")).unwrap(),
        meta_of(MERGING)
    );
    assert_eq!(linked.files, 2);
}

#[test]
fn a_staging_directory_removed_under_the_links_is_not_made_again() {
    let root = scratch("link");
    write(&root.join("cold/2026-11-01/1/shop.dlc"), "cold");
    let into = staging(&root);
    fs::remove_dir_all(root.join(".backup")).unwrap();
    let e = logs(&root, &into).unwrap_err();
    assert_eq!(
        format!("{e:#}").split(':').next().unwrap(),
        format!("creating {}", into.display())
    );
    assert!(!root.join(".backup").exists());
}
