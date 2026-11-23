use super::*;
use crate::items::session::{Session, add_session};

/// 2024-10-02T07:00:00Z.
const HOUR: Timestamp = 1_727_852_400_000;
const CRASHED: i64 = 2 | 4;

fn memory() -> Connection {
    let mut c = Connection::open_in_memory().unwrap();
    crate::store::init(&mut c).unwrap();
    crate::store::create_project(&c, "shop").unwrap();
    crate::store::create_project(&c, "admin").unwrap();
    c
}

fn start(
    c: &Connection,
    project: i64,
    release: &str,
    sid: u8,
    user: &str,
    flags: i64,
    at: Timestamp,
) {
    let s = Session {
        sid: [sid; 16],
        started: at,
        flags,
        user: Some(user.to_owned()),
        release: release.to_owned(),
        environment: "production".to_owned(),
    };
    assert_eq!(add_session(c, project, at, &s).unwrap(), None);
}

#[test]
fn releases_returns_crash_free_sessions_and_users_newest_release_first() {
    let c = memory();
    start(&c, 1, "1.0", 1, "ana", 0, HOUR);
    start(&c, 1, "1.0", 2, "ana", CRASHED, HOUR + 86_400_000);
    start(&c, 1, "1.0", 3, "bo", 0, HOUR + 86_400_000);
    start(&c, 1, "1.1", 4, "ana", 0, HOUR + 2 * 86_400_000);
    start(&c, 2, "9", 5, "cy", 0, HOUR);
    let shop = releases(&c, Some(1), HOUR).unwrap();
    assert_eq!(
        shop,
        vec![
            ReleaseHealth {
                project: "shop".to_owned(),
                release: "1.1".to_owned(),
                environment: "production".to_owned(),
                sessions: 1,
                errored: 0,
                crashed: 0,
                abnormal: 0,
                crash_free_sessions: Some(100.0),
                users: 1,
                crashed_users: 0,
                crash_free_users: Some(100.0),
                first_hour: "2024-10-04T07:00:00Z".to_owned(),
                last_hour: "2024-10-04T07:00:00Z".to_owned(),
            },
            ReleaseHealth {
                project: "shop".to_owned(),
                release: "1.0".to_owned(),
                environment: "production".to_owned(),
                sessions: 3,
                errored: 1,
                crashed: 1,
                abnormal: 0,
                crash_free_sessions: Some(66.66),
                users: 2,
                crashed_users: 1,
                crash_free_users: Some(50.0),
                first_hour: "2024-10-02T07:00:00Z".to_owned(),
                last_hour: "2024-10-03T07:00:00Z".to_owned(),
            },
        ]
    );
    assert_eq!(releases(&c, None, HOUR).unwrap().len(), 3);
    let recent = releases(&c, Some(1), HOUR + 86_400_000).unwrap();
    assert_eq!(
        recent
            .iter()
            .map(|r| (r.release.as_str(), r.sessions, r.users))
            .collect::<Vec<_>>(),
        vec![("1.1", 1, 1), ("1.0", 2, 2)]
    );
}

#[test]
fn releases_without_users_have_no_crash_free_users() {
    let c = memory();
    c.execute(
        "INSERT INTO release_sessions VALUES (1, ?1, 'api@2', 'production', 1000, 3, 1, 0)",
        [HOUR],
    )
    .unwrap();
    let r = &releases(&c, None, HOUR).unwrap()[0];
    assert_eq!(
        (r.crash_free_sessions, r.users, r.crash_free_users),
        (Some(99.9), 0, None)
    );
}
