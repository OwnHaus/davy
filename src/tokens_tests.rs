use super::{
    Change, KEEP_REVOKED_MS, MAX_LIVE, Name, PREFIX, TOUCH_MS, apply, create, find, hash, list,
    one, prune,
};
use crate::access::{Grant, Level};

fn memory() -> rusqlite::Connection {
    let mut c = rusqlite::Connection::open_in_memory().unwrap();
    crate::store::init(&mut c).unwrap();
    c
}

const T: i64 = 1_800_000_000_000;

fn name(s: &str) -> Name {
    Name::parse(s).unwrap()
}

/// Adds `n` at `level` and returns its secret.
fn add(c: &rusqlite::Connection, n: &str, level: Level) -> String {
    create(c, &name(n), level, T).unwrap().unwrap().1
}

fn level(n: &str, level: Level) -> Change {
    Change::Level {
        name: n.into(),
        level,
    }
}

fn unlock(n: &str) -> Change {
    Change::Unlock {
        name: n.into(),
        by: "ann@example.com".into(),
        at: T + 5,
    }
}

#[test]
fn names_are_lowercase_words_a_shell_reads_as_is() {
    for ok in ["ci", "claude-code", "a.b_c-9", "9lives"] {
        assert_eq!(
            Name::parse(ok).map(|n| n.as_str().to_owned()),
            Ok(ok.to_owned())
        );
    }
    let refusal =
        "a token name is 1 to 64 of a-z, 0-9, '.', '_' and '-', starting with a letter or a digit";
    for bad in [
        "",
        "-x",
        ".x",
        "Ci",
        "ci bot",
        "ci/bot",
        "ci\u{202e}",
        &"a".repeat(65),
    ] {
        assert_eq!(Name::parse(bad), Err(refusal.to_owned()), "{bad:?}");
    }
    assert!(Name::parse(&"a".repeat(64)).is_ok());
}

#[test]
fn a_new_token_is_found_by_its_secret_and_only_its_hash_is_kept() {
    let c = memory();
    let (listed, secret) = create(&c, &name("ci"), Level::Triage, T).unwrap().unwrap();
    assert!(secret.starts_with(PREFIX));
    assert_eq!(secret.len(), PREFIX.len() + 43);
    assert_eq!(listed.name, "ci");
    assert_eq!(listed.level, Level::Triage);
    assert_eq!(listed.created_at, "2027-01-15T08:00:00Z");
    assert_eq!(listed.used_at, None);
    assert!(!listed.destructive);
    let kept: Vec<u8> = c
        .query_row("SELECT hash FROM agent_tokens", [], |r| r.get(0))
        .unwrap();
    assert_eq!(kept, hash(&secret).to_vec());
    let found = find(&c, &hash(&secret), T).unwrap().unwrap();
    assert_eq!(found.name, "ci");
    assert_eq!(
        found.grant,
        Grant {
            level: Level::Triage,
            destructive: false
        }
    );
    assert!(found.stale);
    assert_eq!(find(&c, &hash("davy_agent_wrong"), T).unwrap(), None);
}

#[test]
fn a_taken_name_and_the_fifty_first_token_are_refused() {
    let c = memory();
    add(&c, "ci", Level::Read);
    assert_eq!(
        create(&c, &name("ci"), Level::Full, T).unwrap(),
        Err(
            "a token named \"ci\" exists or was revoked less than 180 days ago: pick another name"
                .to_owned()
        )
    );
    for i in 1..MAX_LIVE {
        add(&c, &format!("t{i}"), Level::Read);
    }
    assert_eq!(
        create(&c, &name("one-more"), Level::Read, T).unwrap(),
        Err("there are 50 tokens already: revoke one with davy token revoke".to_owned())
    );
}

#[test]
fn use_is_written_at_most_once_a_minute() {
    let c = memory();
    let secret = add(&c, "ci", Level::Read);
    let id = find(&c, &hash(&secret), T).unwrap().unwrap().id;
    assert_eq!(apply(&c, &Change::Used { id, at: T }).unwrap(), Ok(()));
    assert!(
        !find(&c, &hash(&secret), T + TOUCH_MS - 1)
            .unwrap()
            .unwrap()
            .stale
    );
    assert!(
        find(&c, &hash(&secret), T + TOUCH_MS)
            .unwrap()
            .unwrap()
            .stale
    );
    assert_eq!(
        one(&c, "ci").unwrap().unwrap().used_at.as_deref(),
        Some("2027-01-15T08:00:00Z")
    );
}

#[test]
fn only_a_full_token_is_unlocked_and_lowering_it_locks_it() {
    let c = memory();
    add(&c, "ops", Level::Triage);
    assert_eq!(
        apply(&c, &unlock("ops")).unwrap(),
        Err("token \"ops\" has level triage: only a token of level full can be unlocked for destructive changes".to_owned())
    );
    assert_eq!(apply(&c, &level("ops", Level::Full)).unwrap(), Ok(()));
    assert_eq!(apply(&c, &unlock("ops")).unwrap(), Ok(()));
    let t = one(&c, "ops").unwrap().unwrap();
    assert!(t.destructive);
    assert_eq!(t.unlocked_by.as_deref(), Some("ann@example.com"));
    assert_eq!(t.unlocked_at.as_deref(), Some("2027-01-15T08:00:00.005Z"));
    assert_eq!(apply(&c, &level("ops", Level::Triage)).unwrap(), Ok(()));
    let t = one(&c, "ops").unwrap().unwrap();
    assert_eq!(
        (t.level, t.destructive, t.unlocked_by),
        (Level::Triage, false, None)
    );
    // The table refuses an unlock below full whoever writes it.
    let raw = c.execute("UPDATE agent_tokens SET unlocked_at=1 WHERE name='ops'", []);
    assert!(raw.is_err());
}

#[test]
fn lock_and_revoke_take_effect_on_the_next_lookup() {
    let c = memory();
    let secret = add(&c, "ops", Level::Full);
    apply(&c, &unlock("ops")).unwrap().unwrap();
    assert!(
        find(&c, &hash(&secret), T)
            .unwrap()
            .unwrap()
            .grant
            .destructive
    );
    let lock = Change::Lock { name: "ops".into() };
    assert_eq!(apply(&c, &lock).unwrap(), Ok(()));
    assert!(
        !find(&c, &hash(&secret), T)
            .unwrap()
            .unwrap()
            .grant
            .destructive
    );
    let revoke = Change::Revoke {
        name: "ops".into(),
        at: T + 9,
    };
    assert_eq!(apply(&c, &revoke).unwrap(), Ok(()));
    assert_eq!(find(&c, &hash(&secret), T).unwrap(), None);
    assert_eq!(
        apply(&c, &revoke).unwrap(),
        Err("no token named \"ops\", or it was revoked".to_owned())
    );
    assert_eq!(
        apply(&c, &level("ops", Level::Full)).unwrap(),
        Err("no token named \"ops\", or it was revoked".to_owned())
    );
    assert_eq!(
        apply(&c, &unlock("nobody")).unwrap(),
        Err("no token named \"nobody\", or it was revoked".to_owned())
    );
}

#[test]
fn the_list_puts_live_tokens_first_and_the_sweep_frees_old_names() {
    let c = memory();
    add(&c, "old", Level::Read);
    add(&c, "ci", Level::Full);
    let revoke = Change::Revoke {
        name: "old".into(),
        at: T,
    };
    apply(&c, &revoke).unwrap().unwrap();
    let names: Vec<(String, bool)> = list(&c)
        .unwrap()
        .into_iter()
        .map(|t| (t.name, t.revoked_at.is_some()))
        .collect();
    assert_eq!(names, [("ci".to_owned(), false), ("old".to_owned(), true)]);
    prune(&c, T + KEEP_REVOKED_MS).unwrap();
    assert_eq!(list(&c).unwrap().len(), 2);
    prune(&c, T + KEEP_REVOKED_MS + 1).unwrap();
    let left: Vec<String> = list(&c).unwrap().into_iter().map(|t| t.name).collect();
    assert_eq!(left, ["ci"]);
    assert!(create(&c, &name("old"), Level::Read, T).unwrap().is_ok());
}

/// The proof of a call that waited for the writer while a person locked
/// its token: the writer reads the unlock again and makes no change.
#[test]
fn a_proof_from_before_a_lock_or_revoke_changes_nothing() {
    use crate::{
        access::{Actor, Caller},
        drops::{self, AgentChange, By as DropBy, Spec},
        retention::{self, By, Defaults, Setting},
    };
    let c = memory();
    let (api, _) = crate::store::create_project(&c, "api").unwrap();
    let spec = Spec::new(None, None, Some("noise"), None, None).unwrap();
    let rule = drops::add(&c, api, &spec, false, DropBy::Agent, T)
        .unwrap()
        .unwrap()
        .id;
    let lock = Change::Lock { name: "ops".into() };
    let revoke = Change::Revoke {
        name: "ci".into(),
        at: T + 6,
    };
    for (n, last) in [("ops", lock), ("ci", revoke)] {
        let secret = add(&c, n, Level::Full);
        apply(&c, &unlock(n)).unwrap().unwrap();
        let found = find(&c, &hash(&secret), T).unwrap().unwrap();
        let caller = Caller {
            actor: Actor::Token {
                id: found.id,
                name: found.name,
            },
            grant: found.grant,
        };
        let proof = caller.unlocked().unwrap();
        apply(&c, &last).unwrap().unwrap();
        let shorter = retention::Change {
            project: "api".into(),
            logs_days: Some(Setting::Days(1)),
            errors_days: None,
        };
        let refused = retention::set(&c, &shorter, By::Unlocked(proof, Defaults::default()));
        assert_eq!(
            refused.unwrap().err().as_deref(),
            Some(super::NO_LONGER_UNLOCKED)
        );
        let live = AgentChange::Live {
            project: "api".into(),
            rule,
            at_ms: T,
            unlocked: proof,
        };
        let refused = drops::apply(&c, &live).unwrap();
        assert_eq!(refused.err().as_deref(), Some(super::NO_LONGER_UNLOCKED));
        assert!(!drops::get(&c, rule).unwrap().unwrap().live);
    }
}
