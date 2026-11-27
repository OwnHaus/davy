use super::{Actor, Caller, DESTRUCTIVE, Grant, Level, Need, need};

const READ: Need = Need {
    level: Level::Read,
    destructive: false,
};
const TRIAGE: Need = Need {
    level: Level::Triage,
    destructive: false,
};
const FULL: Need = Need {
    level: Level::Full,
    destructive: false,
};

fn grant(level: Level, destructive: bool) -> Grant {
    Grant { level, destructive }
}

fn token(name: &str, level: Level, destructive: bool) -> Caller {
    Caller {
        actor: Actor::Token {
            id: 1,
            name: name.into(),
        },
        grant: grant(level, destructive),
    }
}

#[test]
fn every_tool_has_the_need_of_the_design_table() {
    let read = [
        "schema",
        "sql",
        "search_logs",
        "count_logs",
        "log_patterns",
        "connect_project",
        "project_status",
        "usage",
        "drop_rules",
        "release_health",
        "issue_context",
    ];
    for tool in read {
        assert_eq!(need(tool), Some(READ), "{tool}");
    }
    for tool in [
        "mark_fixed",
        "submit_feedback",
        "drop_rule_shadow",
        "drop_rule_remove",
    ] {
        assert_eq!(need(tool), Some(TRIAGE), "{tool}");
    }
    assert_eq!(need("set_retention"), Some(FULL));
    assert_eq!(need("drop_rule_live"), Some(DESTRUCTIVE));
    assert_eq!(need("delete_everything"), None);
    assert_eq!(need(""), None);
}

#[test]
fn a_level_holds_the_ones_below_it_and_never_destructive_unlocked() {
    let cases = [
        (grant(Level::Read, false), [true, false, false, false]),
        (grant(Level::Triage, false), [true, true, false, false]),
        (grant(Level::Full, false), [true, true, true, false]),
        (grant(Level::Full, true), [true, true, true, true]),
        (Grant::ADMIN, [true, true, true, false]),
    ];
    for (g, want) in cases {
        let got = [READ, TRIAGE, FULL, DESTRUCTIVE].map(|n| g.admits(n));
        assert_eq!(got, want, "{g:?}");
    }
}

#[test]
fn only_an_unlocked_full_token_makes_the_proof_and_it_names_the_token() {
    let proof = token("ops", Level::Full, true).unlocked();
    assert_eq!(proof.map(crate::access::Unlocked::token), Some(1));
    assert!(token("ops", Level::Full, false).unlocked().is_none());
    assert!(Caller::admin().unlocked().is_none());
    let admin_unlocked = Caller {
        grant: grant(Level::Full, true),
        ..Caller::admin()
    };
    assert!(admin_unlocked.unlocked().is_none());
    // The database keeps an unlock only on a full token; a grant that says
    // otherwise still reaches no destructive change.
    assert!(token("ops", Level::Triage, true).unlocked().is_none());
}

#[test]
fn levels_parse_by_their_names() {
    assert_eq!(Level::parse("read"), Ok(Level::Read));
    assert_eq!(Level::parse("triage"), Ok(Level::Triage));
    assert_eq!(Level::parse("full"), Ok(Level::Full));
    assert_eq!(
        Level::parse("admin"),
        Err("unknown level \"admin\": give read, triage or full".to_owned())
    );
    assert_eq!(serde_json::to_string(&Level::Triage).unwrap(), "\"triage\"");
}

#[test]
fn a_refusal_names_both_levels_and_who_raises_them() {
    assert_eq!(
        token("ci-bot", Level::Read, false).refusal("mark_fixed", TRIAGE),
        "mark_fixed needs level triage, and the token \"ci-bot\" holds read. A person changes a token's level, and unlocks destructive changes, on the Tokens page of the web UI or with davy token."
    );
    assert_eq!(
        token("ops", Level::Full, false).refusal("drop_rule_live", DESTRUCTIVE),
        "drop_rule_live needs level full with destructive changes unlocked, and the token \"ops\" holds full. A person changes a token's level, and unlocks destructive changes, on the Tokens page of the web UI or with davy token."
    );
    assert_eq!(
        Caller::admin().refusal("set_retention", DESTRUCTIVE),
        "set_retention needs level full with destructive changes unlocked, and the admin token holds full. The admin token never makes a destructive change: a person makes it with the CLI or the web UI, or unlocks it for an agent token of level full."
    );
    assert_eq!(Actor::Admin.name(), "admin token");
    assert_eq!(token("ops", Level::Full, true).actor.name(), "ops");
}
