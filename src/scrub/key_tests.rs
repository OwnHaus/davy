use super::*;
use serde_json::json;

fn text(policy: &Policy, input: &str) -> String {
    let mut text = input.to_owned();
    policy.text(&mut text);
    text
}

#[test]
fn look_alike_letters_in_keys_do_not_hide_a_sensitive_key() {
    // Cyrillic о and А, Greek ο, fullwidth letters, accents and a zero-width
    // space, each in a key that is sensitive once folded.
    let mut value = json!({"passw\u{043e}rd":"hunter2","\u{0410}uthorization":"Basic abc",
        "t\u{03bf}ken":"abc","\u{ff50}\u{ff41}\u{ff53}\u{ff53}\u{ff57}\u{ff4f}\u{ff52}\u{ff44}":"abc",
        "p\u{00e4}ssw\u{00f6}rd":"abc","pass\u{200b}word":"abc","api\u{00ad}_key":"abc",
        "\u{1d42d}\u{1d428}\u{1d424}\u{1d41e}\u{1d427}":"abc","pin\u{0441}ode":"kept"});
    Policy::default().value(&mut value);
    assert_eq!(
        value,
        json!({"passw\u{043e}rd":"[Filtered: sensitive]","\u{0410}uthorization":"[Filtered: sensitive]",
        "t\u{03bf}ken":"[Filtered: sensitive]",
        "\u{ff50}\u{ff41}\u{ff53}\u{ff53}\u{ff57}\u{ff4f}\u{ff52}\u{ff44}":"[Filtered: sensitive]",
        "p\u{00e4}ssw\u{00f6}rd":"[Filtered: sensitive]","pass\u{200b}word":"[Filtered: sensitive]",
        "api\u{00ad}_key":"[Filtered: sensitive]",
        "\u{1d42d}\u{1d428}\u{1d424}\u{1d41e}\u{1d427}":"[Filtered: sensitive]","pin\u{0441}ode":"kept"})
    );
}

#[test]
fn a_look_alike_name_hides_its_value() {
    let mut value = json!({"headers":[{"name":"Auth\u{043e}rization","value":"Basic abc"},
        ["C\u{043e}okie","sid=abc"]]});
    Policy::default().value(&mut value);
    assert_eq!(
        value,
        json!({"headers":[{"name":"Auth\u{043e}rization","value":"[Filtered: sensitive]"},
        ["C\u{043e}okie","[Filtered: sensitive]"]]})
    );
}

#[test]
fn extra_keys_fold_look_alikes_when_stored_and_compared() {
    let policy: Policy =
        serde_json::from_value(json!({"extra_keys":["cust\u{043e}merId"]})).unwrap();
    assert_eq!(policy.extra_keys, HashSet::from(["customerid".to_owned()]));
    let mut value = json!({"cust\u{043e}merid":"c-1","customer_id":"c-2","customer":"kept"});
    policy.value(&mut value);
    assert_eq!(
        value,
        json!({"cust\u{043e}merid":"[Filtered: sensitive]","customer_id":"[Filtered: sensitive]",
        "customer":"kept"})
    );
}

#[test]
fn look_alike_assignment_keys_in_text_are_hidden() {
    assert_eq!(
        text(
            &Policy::default(),
            "login passw\u{043e}rd=hunter2; \u{ff54}\u{ff4f}\u{ff4b}\u{ff45}\u{ff4e}: abc; ok=1"
        ),
        "login passw\u{043e}rd=[Filtered: sensitive]; \u{ff54}\u{ff4f}\u{ff4b}\u{ff45}\u{ff4e}=[Filtered: sensitive]; ok=1"
    );
    assert_eq!(
        text(&Policy::default(), "?passw%D0%BErd=hunter2&ok=1"),
        "?passw%D0%BErd=[Filtered: sensitive]&ok=1"
    );
}

#[test]
fn a_long_key_that_folds_wider_keeps_its_end() {
    let wide = format!("{}_token", "\u{fdfa}".repeat(100));
    let mut value = json!({ wide.clone(): "abc", "\u{fdfa}".repeat(100): "kept" });
    Policy::default().value(&mut value);
    assert_eq!(
        value,
        json!({ wide: "[Filtered: sensitive]", "\u{fdfa}".repeat(100): "kept" })
    );
}

#[test]
fn assignment_keys_encoded_up_to_three_times_are_decoded() {
    assert_eq!(
        text(&Policy::default(), "/login?p%2561ssword=hunter2&ok=1"),
        "/login?p%2561ssword=[Filtered: sensitive]&ok=1"
    );
    assert_eq!(
        text(&Policy::default(), "/login?p%252561ssword=hunter2&ok=1"),
        "/login?p%252561ssword=[Filtered: sensitive]&ok=1"
    );
    assert_eq!(
        text(&Policy::default(), "a=%25%25%25&b=1"),
        "a=%25%25%25&b=1"
    );
}

#[test]
fn extra_keys_apply_with_the_keys_class_off() {
    let policy: Policy =
        serde_json::from_value(json!({"keys":false,"extra_keys":["customer_id"]})).unwrap();
    let mut value = json!({"customerId":"c-1","password":"kept",
        "headers":[{"name":"customer-id","value":"c-2"}],
        "message":"customer_id=c-3&password=kept"});
    policy.value(&mut value);
    assert_eq!(
        value,
        json!({"customerId":"[Filtered: sensitive]","password":"kept",
        "headers":[{"name":"customer-id","value":"[Filtered: sensitive]"}],
        "message":"customer_id=[Filtered: sensitive]&password=kept"})
    );
}
