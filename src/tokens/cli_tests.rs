use super::{Token, run};
use crate::tokens::PREFIX;
use serde_json::{Value, json};

fn out(command: Token) -> anyhow::Result<String> {
    let mut out = Vec::new();
    run(&mut out, command)?;
    Ok(String::from_utf8(out).unwrap())
}

#[test]
fn add_prints_the_secret_once_and_list_never_does() {
    let data = crate::test_support::scratch("token-cli");
    let add = Token::Add {
        name: "ci".into(),
        level: "triage".into(),
        data: data.clone(),
    };
    let printed: Value = serde_json::from_str(&out(add).unwrap()).unwrap();
    let secret = printed["secret"].as_str().unwrap();
    assert!(secret.starts_with(PREFIX), "{secret}");
    assert_eq!(secret.len(), PREFIX.len() + 43);
    assert_eq!(
        (
            &printed["token"]["name"],
            &printed["token"]["level"],
            &printed["token"]["destructive"],
            &printed["token"]["used_at"]
        ),
        (&json!("ci"), &json!("triage"), &json!(false), &Value::Null)
    );
    let list = |json| Token::List {
        data: data.clone(),
        json,
    };
    assert_eq!(out(list(false)).unwrap(), "ci\ttriage\t-\tnever\n");
    let listed = out(list(true)).unwrap();
    assert!(!listed.contains(secret));
    let level = Token::Level {
        name: "ci".into(),
        level: "full".into(),
        data: data.clone(),
    };
    out(level).unwrap();
    let unlock = Token::Unlock {
        name: "ci".into(),
        data: data.clone(),
    };
    let unlocked: Value = serde_json::from_str(&out(unlock).unwrap()).unwrap();
    assert_eq!(
        (
            &unlocked["token"]["destructive"],
            &unlocked["token"]["unlocked_by"]
        ),
        (&json!(true), &json!("cli"))
    );
    assert_eq!(out(list(false)).unwrap(), "ci\tfull\tdestructive\tnever\n");
    let revoke = Token::Revoke {
        name: "ci".into(),
        data: data.clone(),
    };
    out(revoke).unwrap();
    let text = out(list(false)).unwrap();
    assert!(text.starts_with("ci\tfull\t-\tnever\trevoked"), "{text}");
}

#[test]
fn a_change_names_what_is_wrong() {
    let data = crate::test_support::scratch("token-cli-wrong");
    let add = |name: &str, level: &str| Token::Add {
        name: name.into(),
        level: level.into(),
        data: data.clone(),
    };
    assert_eq!(
        out(add("ci", "admin")).unwrap_err().to_string(),
        "unknown level \"admin\": give read, triage or full"
    );
    assert_eq!(
        out(add("CI", "read")).unwrap_err().to_string(),
        "a token name is 1 to 64 of a-z, 0-9, '.', '_' and '-', starting with a letter or a digit"
    );
    out(add("ci", "read")).unwrap();
    let unlock = Token::Unlock {
        name: "ci".into(),
        data: data.clone(),
    };
    assert_eq!(
        out(unlock).unwrap_err().to_string(),
        "token \"ci\" has level read: only a token of level full can be unlocked for destructive changes"
    );
    let revoke = Token::Revoke {
        name: "bot".into(),
        data: data.clone(),
    };
    assert_eq!(
        out(revoke).unwrap_err().to_string(),
        "no token named \"bot\", or it was revoked"
    );
}
