use super::{What, retention, status, token, token_action, token_added, tool};
use crate::{access::Level, store::IssueStatus, tokens::Change};
use serde_json::{Map, Value, json};

fn args(v: Value) -> Map<String, Value> {
    let Value::Object(map) = v else {
        panic!("arguments are an object")
    };
    map
}

fn summary(name: &str, v: Value) -> (Option<String>, String) {
    let w = tool(name, &args(v));
    (w.target, w.summary)
}

#[test]
fn a_resolve_names_its_issue_and_release() {
    assert_eq!(
        tool(
            "mark_fixed",
            &args(json!({"issue_id": 7, "release": "2.0.0"}))
        ),
        What {
            project: None,
            issue: Some(7),
            target: Some("issue 7".into()),
            summary: "resolved, fixed in release 2.0.0".into(),
        }
    );
    assert_eq!(
        summary("mark_fixed", json!({"issue_id": 7})).1,
        "resolved, fixed in the next release"
    );
}

#[test]
fn a_drop_rule_names_what_it_matches() {
    assert_eq!(
        summary(
            "drop_rule_shadow",
            json!({"project": "api", "pattern": "0123456789ab", "service": "web", "keep": "1%"})
        ),
        (
            Some("pattern 0123456789ab".into()),
            "added a shadow rule for pattern 0123456789ab, service web, keeping 1%".into()
        )
    );
    assert_eq!(
        summary(
            "drop_rule_shadow",
            json!({"project": "api", "level": "debug"})
        )
        .1,
        "added a shadow rule for level debug and below"
    );
    assert_eq!(
        summary(
            "drop_rule_shadow",
            json!({"project": "api", "message": "GET /health"})
        )
        .1,
        "added a shadow rule for message \"GET /health\""
    );
    assert_eq!(
        summary("drop_rule_remove", json!({"project": "api", "rule_id": 3})),
        (Some("rule 3".into()), "removed shadow rule 3".into())
    );
    assert_eq!(
        summary("drop_rule_live", json!({"project": "api", "rule_id": 3})),
        (Some("rule 3".into()), "made rule 3 live".into())
    );
    assert_eq!(
        tool(
            "drop_rule_live",
            &args(json!({"project": "api", "rule_id": 3}))
        )
        .project,
        Some("api".into())
    );
}

#[test]
fn a_retention_change_says_both_settings() {
    assert_eq!(
        summary(
            "set_retention",
            json!({"project": "api", "logs_days": 90, "errors_days": "default"})
        )
        .1,
        "logs 90 days, errors the default"
    );
    assert_eq!(
        summary("set_retention", json!({"project": "api", "errors_days": 7})).1,
        "logs unchanged, errors 7 days"
    );
    assert_eq!(
        summary(
            "set_retention",
            json!({"project": "api", "logs_days": "a lot"})
        )
        .1,
        "changed retention"
    );
    let change = retention::Change {
        project: "web".into(),
        logs_days: Some(retention::Setting::Days(3)),
        errors_days: None,
    };
    assert_eq!(
        retention(&change),
        What {
            project: Some("web".into()),
            issue: None,
            target: None,
            summary: "logs 3 days, errors unchanged".into(),
        }
    );
}

#[test]
fn feedback_keeps_none_of_its_text() {
    assert_eq!(
        summary(
            "submit_feedback",
            json!({"tried": "secret stuff", "happened": "x"})
        ),
        (None, "sent feedback about Davy".into())
    );
}

#[test]
fn a_bulk_status_change_lists_the_first_issues() {
    let ids: Vec<i64> = (1..=8).collect();
    assert_eq!(
        status(&ids, &IssueStatus::Ignored),
        What {
            project: None,
            issue: None,
            target: Some("8 issues".into()),
            summary: "ignored: issues 1, 2, 3, 4, 5, and 3 more".into(),
        }
    );
    assert_eq!(
        status(&[4, 9], &IssueStatus::Unresolved).summary,
        "reopened: issues 4, 9"
    );
    assert_eq!(
        status(&[4], &IssueStatus::Unresolved).target.as_deref(),
        Some("issue 4")
    );
}

#[test]
fn a_release_is_cut_to_forty_characters() {
    let resolved = IssueStatus::Resolved {
        in_release: Some("r".repeat(50)),
        at: 0,
    };
    assert_eq!(
        status(&[1], &resolved).summary,
        format!("resolved, fixed in release {}…", "r".repeat(39))
    );
}

#[test]
fn a_token_change_names_the_token() {
    let unlock = Change::Unlock {
        name: "ci".into(),
        by: "ann@example.com".into(),
        at: 0,
    };
    assert_eq!(
        (token_action(&unlock), token(&unlock)),
        (
            "token_unlock",
            What {
                project: None,
                issue: None,
                target: Some("token ci".into()),
                summary: "unlocked destructive changes".into(),
            }
        )
    );
    let level = Change::Level {
        name: "ci".into(),
        level: Level::Triage,
    };
    assert_eq!(
        (token_action(&level), token(&level).summary.as_str()),
        ("token_level", "set level triage")
    );
    let revoke = Change::Revoke {
        name: "ci".into(),
        at: 0,
    };
    assert_eq!(token(&revoke).summary, "revoked");
    assert_eq!(
        token(&Change::Lock { name: "ci".into() }).summary,
        "locked destructive changes"
    );
    assert_eq!(
        token_added("ci", Level::Full).summary,
        "added at level full"
    );
}
