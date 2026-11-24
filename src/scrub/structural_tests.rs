use super::*;
use serde_json::json;

fn session(policy: &Policy, mut value: Value) -> Value {
    policy.item(&mut value, Item::Session, &mut Meter::unmetered());
    value
}

#[test]
fn structural_names_below_the_root_are_free_text() {
    let mut value = json!({"extra":{"platform":"password=hunter2; AKIAIOSFODNN7EXAMPLE",
        "logger":"4242424242424242",
        "release":"Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2ln"},
        "tags":[["dist","4242424242424242"]],
        "breadcrumbs":{"values":[{"data":{"level":"192.0.2.1"}}]},
        "contexts":{"app":{"environment":"192.0.2.2"},"trace":{"release":"192.0.2.3"}}});
    Policy::default().value(&mut value);
    assert_eq!(
        value,
        json!({"extra":{"platform":"password=[Filtered: sensitive]; [Filtered: credential]",
        "logger":"[Filtered: card]",
        "release":"[Filtered: credential]"},
        "tags":[["dist","[Filtered: card]"]],
        "breadcrumbs":{"values":[{"data":{"level":"[Filtered: ip]"}}]},
        "contexts":{"app":{"environment":"[Filtered: ip]"},"trace":{"release":"192.0.2.3"}}})
    );
}

#[test]
fn root_structural_fields_still_lose_credentials_and_assignments() {
    let mut value = json!({"release":"password=x","logger":"ghp_abcdefghij",
        "platform":"Bearer abc","environment":"token: s3cret",
        "level":"4242424242424242","dist":"1.2.3.4","monitor_slug":"pwd=1&ok",
        "event_id":"-----BEGIN PRIVATE KEY-----abc"});
    Policy {
        email: true,
        ..Policy::default()
    }
    .value(&mut value);
    assert_eq!(
        value,
        json!({"release":"password=[Filtered: sensitive]","logger":"[Filtered: credential]",
        "platform":"[Filtered: credential]","environment":"token=[Filtered: sensitive]",
        "level":"4242424242424242","dist":"1.2.3.4","monitor_slug":"pwd=[Filtered: sensitive]&ok",
        "event_id":"[Filtered: private key]"})
    );
}

#[test]
fn session_aggregates_keep_their_release_and_user_ids() {
    let policy = Policy {
        email: true,
        ..Policy::default()
    };
    let aggregates = json!({"aggregates":[
        {"started":"2024-10-02T07:14:00.000Z","did":"ana@example.com","exited":1},
        {"started":"2024-10-02T07:14:00.000Z","did":"password=x","exited":1}],
        "attrs":{"release":"api@1.2.3.4","environment":"10.0.0.1","ip_address":"192.0.2.7"}});
    assert_eq!(
        session(&policy, aggregates),
        json!({"aggregates":[
        {"started":"2024-10-02T07:14:00.000Z","did":"ana@example.com","exited":1},
        {"started":"2024-10-02T07:14:00.000Z","did":"password=[Filtered: sensitive]","exited":1}],
        "attrs":{"release":"api@1.2.3.4","environment":"10.0.0.1","ip_address":"[Filtered: ip]"}})
    );
}

#[test]
fn session_ids_are_kept_only_where_a_session_keeps_them() {
    let policy = Policy::default();
    let nested = json!({"sid":"10.0.0.1","extra":{"did":"10.0.0.2"},
        "attrs":{"did":"10.0.0.3","user":{"sid":"10.0.0.4"}},
        "aggregates":[[{"did":"10.0.0.5"}]]});
    assert_eq!(
        session(&policy, nested),
        json!({"sid":"10.0.0.1","extra":{"did":"[Filtered: ip]"},
        "attrs":{"did":"10.0.0.3","user":{"sid":"[Filtered: ip]"}},
        "aggregates":[[{"did":"[Filtered: ip]"}]]})
    );
    let mut event = json!({"attrs":{"release":"1.2.3.4"},"aggregates":[{"did":"10.0.0.6"}]});
    policy.value(&mut event);
    assert_eq!(
        event,
        json!({"attrs":{"release":"[Filtered: ip]"},"aggregates":[{"did":"[Filtered: ip]"}]})
    );
}
