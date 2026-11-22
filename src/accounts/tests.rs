use super::backoff::{Backoff, MAX_WAIT};
use super::store::{self, Change, LINK_USED, PASSWORD_CHANGED, Session};
use super::{
    DUMMY_HASH, Email, INVITE_MS, Person, SESSION_IDLE_MS, SESSION_MAX_MS, Secret, SecretHash,
    TOUCH_EVERY_MS, check_new_password, hash_password, verify_password,
};
use argon2::password_hash::PasswordVerifier as _;
use std::time::{Duration, Instant};

fn memory() -> rusqlite::Connection {
    let mut c = rusqlite::Connection::open_in_memory().unwrap();
    crate::store::init(&mut c).unwrap();
    c
}

fn email(s: &str) -> Email {
    Email::parse(s).unwrap()
}

fn h(byte: u8) -> SecretHash {
    [byte; 32]
}

const T: i64 = 1_800_000_000_000;

/// A user `ann@example.com` with the password hash "p1" and a session `h(9)`.
fn signed_in(c: &rusqlite::Connection) -> i64 {
    let id = store::add(c, &email("ann@example.com"), &h(1), T)
        .unwrap()
        .unwrap();
    let welcome = Change::Welcome {
        invite: h(1),
        password: "p1".into(),
        session: h(9),
        at: T,
    };
    assert_eq!(store::apply(c, &welcome).unwrap(), Ok(()));
    id
}

#[test]
fn an_email_is_trimmed_and_lowercased_and_must_look_like_one() {
    assert_eq!(email("  Ann@Example.COM ").as_str(), "ann@example.com");
    let long = format!("{}@example.com", "a".repeat(250));
    for bad in [
        "",
        "ann",
        "@example.com",
        "ann@localhost",
        "ann@@example.com",
        "a@b@example.com",
        "ann smith@example.com",
        "ann@exa\u{7}mple.com",
        long.as_str(),
    ] {
        assert!(Email::parse(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn a_new_password_needs_12_characters_and_at_most_1024_bytes() {
    assert!(check_new_password("elevenchars").is_err());
    assert_eq!(check_new_password("twelve chars"), Ok(()));
    assert_eq!(check_new_password("ěščřžýáíéúůď"), Ok(()));
    assert!(check_new_password(&"a".repeat(1025)).is_err());
}

#[test]
fn a_password_matches_only_its_own_hash() {
    let hash = hash_password("correct horse battery").unwrap();
    assert!(hash.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"));
    assert_ne!(hash, hash_password("correct horse battery").unwrap());
    assert!(verify_password(Some(&hash), "correct horse battery"));
    assert!(!verify_password(Some(&hash), "correct horse batterY"));
    assert!(!verify_password(None, "correct horse battery"));
}

#[test]
fn the_dummy_hash_costs_what_a_real_one_costs() {
    let params = |hash: &str| hash.rsplitn(3, '$').nth(2).unwrap().to_owned();
    assert_eq!(params(DUMMY_HASH), params(&hash_password("x").unwrap()));
    let checked = super::argon2().verify_password(b"x", DUMMY_HASH);
    assert_eq!(checked, Err(argon2::password_hash::Error::PasswordInvalid));
}

#[test]
fn a_secret_survives_its_encoding_and_nothing_else_parses() {
    let s = Secret::random().unwrap();
    let encoded = s.encode();
    assert_eq!(encoded.len(), 43);
    assert_eq!(Secret::parse(&encoded).unwrap().hash(), s.hash());
    assert_ne!(Secret::random().unwrap().hash(), s.hash());
    for bad in [
        "",
        "abc",
        &encoded[..42],
        &format!("{encoded}A"),
        "!".repeat(43).as_str(),
    ] {
        assert!(Secret::parse(bad).is_none(), "{bad:?}");
    }
}

#[test]
fn a_welcome_link_sits_under_the_public_url() {
    let s = Secret::parse(&"A".repeat(43)).unwrap();
    for (base, link) in [
        (
            "https://davy.example.com",
            "https://davy.example.com/ui/welcome#",
        ),
        (
            "https://example.com/davy/",
            "https://example.com/davy/ui/welcome#",
        ),
    ] {
        let url = url::Url::parse(base).unwrap();
        assert_eq!(
            super::cli::welcome_link(&url, &s),
            format!("{link}{}", "A".repeat(43))
        );
    }
}

#[test]
fn adding_a_user_twice_keeps_the_first() {
    let c = memory();
    let ann = email("ann@example.com");
    let id = store::add(&c, &ann, &h(1), T).unwrap().unwrap();
    assert_eq!(store::add(&c, &ann, &h(2), T).unwrap(), None);
    assert!(store::invite_works(&c, &h(1), T).unwrap());
    assert!(!store::invite_works(&c, &h(2), T).unwrap());
    let users = store::list(&c).unwrap();
    assert_eq!(
        users,
        vec![store::User {
            id,
            email: "ann@example.com".into(),
            active: false,
            created_at: T,
        }]
    );
    assert_eq!(store::password(&c, &ann).unwrap(), Some((id, None)));
}

#[test]
fn a_welcome_link_works_once_and_for_a_day() {
    let c = memory();
    store::add(&c, &email("ann@example.com"), &h(1), T).unwrap();
    store::add(&c, &email("bob@example.com"), &h(2), T).unwrap();
    let welcome = |invite, session, at| Change::Welcome {
        invite,
        password: "p".into(),
        session,
        at,
    };
    let late = welcome(h(2), h(8), T + INVITE_MS);
    assert_eq!(store::apply(&c, &late).unwrap(), Err(LINK_USED.into()));
    assert_eq!(store::apply(&c, &welcome(h(1), h(9), T)).unwrap(), Ok(()));
    assert_eq!(
        store::apply(&c, &welcome(h(1), h(7), T)).unwrap(),
        Err(LINK_USED.into())
    );
    let users = store::list(&c).unwrap();
    assert_eq!(
        users.iter().map(|u| u.active).collect::<Vec<_>>(),
        [true, false]
    );
    assert!(store::session(&c, &h(9), T).unwrap().is_some());
    assert!(store::session(&c, &h(7), T).unwrap().is_none());
}

#[test]
fn a_session_ends_after_14_idle_days_or_90_days() {
    let c = memory();
    let id = signed_in(&c);
    let ann = Person {
        id,
        email: email("ann@example.com"),
    };
    let at = |now| store::session(&c, &h(9), now).unwrap();
    assert_eq!(
        at(T),
        Some(Session {
            person: ann.clone(),
            stale: false
        })
    );
    assert!(at(T + TOUCH_EVERY_MS).unwrap().stale);
    assert_eq!(at(T + SESSION_IDLE_MS), None);
    let mut used = T;
    while used + SESSION_IDLE_MS / 2 < T + SESSION_MAX_MS {
        used += SESSION_IDLE_MS / 2;
        let touch = Change::Touch {
            session: h(9),
            at: used,
        };
        assert_eq!(store::apply(&c, &touch).unwrap(), Ok(()));
        assert_eq!(at(used).unwrap().person, ann);
    }
    assert_eq!(at(T + SESSION_MAX_MS), None);
}

#[test]
fn a_login_after_a_reset_starts_no_session() {
    let c = memory();
    let id = signed_in(&c);
    let login = |session| Change::Login {
        user: id,
        password: "p1".into(),
        session,
        at: T,
    };
    assert_eq!(store::apply(&c, &login(h(10))).unwrap(), Ok(()));
    assert!(store::reset(&c, &email("ann@example.com"), &h(3), T).unwrap());
    assert_eq!(
        store::apply(&c, &login(h(11))).unwrap(),
        Err(PASSWORD_CHANGED.into())
    );
    for session in [h(9), h(10), h(11)] {
        assert_eq!(store::session(&c, &session, T).unwrap(), None);
    }
    assert_eq!(store::password_of(&c, id).unwrap(), None);
    assert!(store::invite_works(&c, &h(3), T).unwrap());
    assert!(!store::invite_works(&c, &h(1), T).unwrap());
}

#[test]
fn a_new_password_ends_the_other_sessions_unless_it_changed_meanwhile() {
    let c = memory();
    let id = signed_in(&c);
    let login = Change::Login {
        user: id,
        password: "p1".into(),
        session: h(10),
        at: T,
    };
    assert_eq!(store::apply(&c, &login).unwrap(), Ok(()));
    let set = |old: &str, new: &str| Change::SetPassword {
        user: id,
        old: old.into(),
        new: new.into(),
        keep: h(9),
    };
    assert_eq!(
        store::apply(&c, &set("p0", "p2")).unwrap(),
        Err(PASSWORD_CHANGED.into())
    );
    assert!(store::session(&c, &h(10), T).unwrap().is_some());
    assert_eq!(store::apply(&c, &set("p1", "p2")).unwrap(), Ok(()));
    assert_eq!(store::password_of(&c, id).unwrap().as_deref(), Some("p2"));
    assert!(store::session(&c, &h(9), T).unwrap().is_some());
    assert!(store::session(&c, &h(10), T).unwrap().is_none());
}

#[test]
fn logging_out_ends_one_session_and_logging_out_others_keeps_it() {
    let c = memory();
    let id = signed_in(&c);
    for session in [h(10), h(11)] {
        let login = Change::Login {
            user: id,
            password: "p1".into(),
            session,
            at: T,
        };
        assert_eq!(store::apply(&c, &login).unwrap(), Ok(()));
    }
    let out = Change::Logout { session: h(11) };
    assert_eq!(store::apply(&c, &out).unwrap(), Ok(()));
    let others = Change::LogoutOthers {
        user: id,
        keep: h(9),
    };
    assert_eq!(store::apply(&c, &others).unwrap(), Ok(()));
    let live: Vec<bool> = [h(9), h(10), h(11)]
        .iter()
        .map(|s| store::session(&c, s, T).unwrap().is_some())
        .collect();
    assert_eq!(live, [true, false, false]);
}

#[test]
fn removing_a_user_ends_their_sessions() {
    let c = memory();
    signed_in(&c);
    assert!(store::remove(&c, &email("ann@example.com")).unwrap());
    assert!(!store::remove(&c, &email("ann@example.com")).unwrap());
    assert_eq!(store::session(&c, &h(9), T).unwrap(), None);
    let rows: i64 = c
        .query_row("SELECT count(*) FROM sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 0);
}

#[test]
fn the_sweep_deletes_ended_sessions_and_links() {
    let c = memory();
    signed_in(&c);
    store::add(&c, &email("bob@example.com"), &h(2), T).unwrap();
    let count = |table: &str| -> i64 {
        c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    };
    store::sweep(&c, T + INVITE_MS - 1).unwrap();
    assert_eq!((count("sessions"), count("invites")), (1, 1));
    store::sweep(&c, T + SESSION_IDLE_MS).unwrap();
    assert_eq!((count("sessions"), count("invites")), (0, 0));
}

#[test]
fn the_wait_doubles_per_attempt_up_to_15_minutes() {
    let mut b = Backoff::new(10);
    let ann = email("ann@example.com");
    let mut now = Instant::now();
    let mut waits = vec![];
    for _ in 0..12 {
        b.attempt(&ann, now).unwrap();
        let wait = b.attempt(&ann, now).unwrap_err();
        waits.push(wait.as_secs());
        now += wait;
    }
    assert_eq!(waits, [1, 2, 4, 8, 16, 32, 64, 128, 256, 512, 900, 900]);
    assert_eq!(MAX_WAIT, Duration::from_mins(15));
}

#[test]
fn a_success_forgets_the_attempts_of_that_email_only() {
    let mut b = Backoff::new(10);
    let (ann, bob) = (email("ann@example.com"), email("bob@example.com"));
    let now = Instant::now();
    b.attempt(&ann, now).unwrap();
    b.attempt(&bob, now).unwrap();
    b.succeeded(&ann);
    assert_eq!(b.attempt(&ann, now), Ok(()));
    assert_eq!(b.attempt(&bob, now), Err(Duration::from_secs(1)));
}

#[test]
fn a_full_backoff_drops_the_entry_whose_wait_ended_first() {
    let mut b = Backoff::new(2);
    let (ann, bob, cid) = (
        email("ann@example.com"),
        email("bob@example.com"),
        email("cid@example.com"),
    );
    let now = Instant::now();
    let later = now + Duration::from_secs(2);
    b.attempt(&ann, now).unwrap();
    b.attempt(&bob, later).unwrap();
    b.attempt(&cid, later).unwrap();
    assert_eq!(b.attempt(&bob, later), Err(Duration::from_secs(1)));
    assert_eq!(b.attempt(&cid, later), Err(Duration::from_secs(1)));
    assert_eq!(b.attempt(&ann, now), Ok(()));
}

#[test]
fn a_flood_of_new_emails_does_not_reset_the_count_of_one_under_attack() {
    let mut b = Backoff::new(3);
    let ann = email("ann@example.com");
    let mut now = Instant::now();
    for _ in 0..5 {
        b.attempt(&ann, now).unwrap();
        now += b.attempt(&ann, now).unwrap_err();
    }
    for i in 0..100 {
        b.attempt(&email(&format!("new{i}@example.com")), now)
            .unwrap();
    }
    b.attempt(&ann, now).unwrap();
    assert_eq!(b.attempt(&ann, now), Err(Duration::from_secs(32)));
}

#[test]
fn user_add_refuses_a_link_to_plain_http_on_another_host() {
    #[derive(clap::Parser)]
    struct Davy {
        #[command(subcommand)]
        user: super::cli::User,
    }
    let data = crate::test_support::scratch("user-add");
    let data = data.to_str().unwrap();
    let add = |email: &str, url: &str| {
        let davy = <Davy as clap::Parser>::try_parse_from([
            "davy",
            "add",
            email,
            "--data",
            data,
            "--public-url",
            url,
        ])
        .unwrap();
        let mut out = Vec::new();
        super::cli::run(&mut out, davy.user).map(|()| String::from_utf8(out).unwrap())
    };
    let e = add("ann@example.com", "http://davy.example.com/").unwrap_err();
    assert!(format!("{e:#}").contains("needs https"), "{e:#}");
    for (email, url) in [
        ("bob@example.com", "https://davy.example.com/"),
        ("cid@example.com", "http://127.0.0.1:8080/"),
        ("dan@example.com", "http://localhost:8080/"),
    ] {
        let link = add(email, url).unwrap();
        assert!(link.starts_with(&format!("{url}ui/welcome#")), "{link}");
    }
}
