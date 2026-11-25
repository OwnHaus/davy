use super::*;
use crate::test_support::scratch;

fn number(c: &Connection, sql: &str) -> Option<i64> {
    c.query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn the_project_is_made_once_with_its_days_and_policy() {
    let data = scratch("own-once");
    let mut c = store::open(&data).unwrap();
    let (own, created) = find_or_create(&mut c).unwrap();
    assert!(created);
    assert_eq!(own.slug, PROJECT);
    assert_eq!(find_or_create(&mut c).unwrap(), (own.clone(), false));
    assert_eq!(find(&c).unwrap(), Some(own.clone()));
    assert_eq!(
        number(
            &c,
            &format!(
                "SELECT log_retention_days FROM projects WHERE id={}",
                own.id
            )
        ),
        Some(LOG_DAYS.into())
    );
    assert_eq!(crate::scrub::settings::read(&c, own.id).unwrap(), policy());
    assert_eq!(
        number(&c, "SELECT count(*) FROM projects WHERE own=1"),
        Some(1)
    );
    drop(c);
    std::fs::remove_dir_all(&data).unwrap();
}

#[test]
fn a_project_davy_a_person_made_stays_theirs() {
    let data = scratch("own-taken");
    let mut c = store::open(&data).unwrap();
    let (theirs, _) = store::create_project(&c, PROJECT).unwrap();
    let (own, created) = find_or_create(&mut c).unwrap();
    assert!(created);
    assert_eq!(own.slug, "davy-self");
    assert_ne!(own.id, theirs);
    assert_eq!(
        number(
            &c,
            &format!("SELECT log_retention_days FROM projects WHERE id={theirs}")
        ),
        None
    );
    assert_eq!(
        crate::scrub::settings::read(&c, theirs).unwrap(),
        Policy::default()
    );
    assert_eq!(
        number(&c, &format!("SELECT own FROM projects WHERE id={theirs}")),
        Some(0)
    );
    drop(c);
    std::fs::remove_dir_all(&data).unwrap();
}

#[test]
fn the_next_free_slug_is_taken() {
    let data = scratch("own-next");
    let mut c = store::open(&data).unwrap();
    for slug in ["davy", "davy-self", "davy-self-2"] {
        store::create_project(&c, slug).unwrap();
    }
    assert_eq!(find_or_create(&mut c).unwrap().0.slug, "davy-self-3");
    drop(c);
    std::fs::remove_dir_all(&data).unwrap();
}

#[test]
fn the_slugs_tried_are_bounded() {
    let all: Vec<String> = slugs().collect();
    assert_eq!(all.len(), 101);
    assert_eq!(&all[..3], ["davy", "davy-self", "davy-self-2"]);
    assert_eq!(all.last().map(String::as_str), Some("davy-self-100"));
}

#[test]
fn a_second_own_project_is_refused_by_the_store() {
    let data = scratch("own-unique");
    let mut c = store::open(&data).unwrap();
    find_or_create(&mut c).unwrap();
    let (other, _) = store::create_project(&c, "api").unwrap();
    assert!(
        c.execute("UPDATE projects SET own=1 WHERE id=?1", [other])
            .is_err()
    );
    drop(c);
    std::fs::remove_dir_all(&data).unwrap();
}

#[test]
fn the_created_line_says_why_the_slug_is_not_davy() {
    assert_eq!(
        created_line("davy"),
        "created project \"davy\" for Davy's own warnings, errors and crashes, keeping 30 days of logs; `davy serve --no-self-report` turns this off"
    );
    assert_eq!(
        created_line("davy-self"),
        "created project \"davy-self\" (a project of a person has the slug \"davy\") for Davy's own warnings, errors and crashes, keeping 30 days of logs; `davy serve --no-self-report` turns this off"
    );
}

#[test]
fn a_store_that_cannot_be_opened_gives_no_project() {
    let file = scratch("own-no-store");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "not a directory").unwrap();
    assert!(open(&file).is_none());
    std::fs::remove_file(&file).unwrap();
}
