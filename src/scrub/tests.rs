use super::*;
use serde_json::json;

#[test]
fn scrubs_sdk_shapes_and_encoded_assignments() {
    let mut value = json!({
        "request": {"headers": [["Authorization", "Basic abc"], ["X-OK", "yes"]],
            "cookies": "sid=ordinary; session=hidden", "query_string": "api%5Fkey=hidden&ok=yes",
            "data": {"Password": "hidden"}},
        "exception": {"values": [{"stacktrace": {"frames": [{"vars": {"csrf": "hidden"}}]}}]},
        "breadcrumbs": {"values": [{"data": {"secret": "hidden"}}]},
        "extra": {"card_number": 4_242_424_242_424_242_u64},
        "contexts": {"custom": {"token": {"nested": "hidden"}}},
        "message": "contact me@example.org at 2001:db8::1 with Bearer hidden"
    });
    Policy::default().value(&mut value);
    let text = value.to_string();
    assert!(!text.contains("hidden"));
    assert_eq!(value["request"]["headers"][0][1], "[Filtered: sensitive]");
    assert_eq!(value["request"]["headers"][1][1], "yes");
    assert_eq!(
        value["message"],
        "contact me@example.org at [Filtered: ip] with [Filtered: credential]"
    );
    let once = value.clone();
    Policy::default().value(&mut value);
    assert_eq!(value, once);
}

#[test]
fn recognizes_credentials_and_luhn_but_keeps_invalid_cards() {
    for text in [
        "ghp_abcdefghijklmnopqrstuvwxyz0123456789",
        "sk-ant-abcdefghijklmnopqrstuvwxyz",
        "sk-proj-abcdefghijklmnopqrstuvwxyz",
        "AKIAIOSFODNN7EXAMPLE",
        "xoxb-123456-abcdef",
        "sk_live_abcdefghijklmnop",
        "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.abcdefgh",
        "-----BEGIN RSA PRIVATE KEY-----\nabc\n-----END RSA PRIVATE KEY-----",
    ] {
        let mut value = text.to_owned();
        Policy::default().text(&mut value);
        assert!(value.starts_with("[Filtered:"), "{value}");
    }
    let mut text = "card 4242 4242 4242 4242 invalid 4242424242424241".to_owned();
    Policy::default().text(&mut text);
    assert_eq!(text, "card [Filtered: card] invalid 4242424242424241");
}

#[test]
fn a_card_or_token_next_to_a_non_ascii_letter_is_still_found() {
    let mut text = "\u{10d}4242424242424242 \u{3a9}ghp_abcdefghij".to_owned();
    Policy::default().text(&mut text);
    assert_eq!(
        text,
        "\u{10d}[Filtered: card] \u{3a9}[Filtered: credential]"
    );
}

#[test]
fn classes_can_be_disabled_and_custom_keys_are_literal() {
    let policy: Policy =
        serde_json::from_value(json!({"ip": false, "extra_keys": ["tenant-pin"]})).unwrap();
    let mut value = json!({"ip": "127.0.0.1", "tenant_pin": ["hidden",123], "mail": "a@b.com"});
    policy.value(&mut value);
    assert_eq!(
        value,
        json!({"ip": "127.0.0.1", "tenant_pin": ["[Filtered: sensitive]",123], "mail": "a@b.com"})
    );
}

#[test]
fn captured_sdk_events_keep_grouping_after_scrubbing() {
    for dir in ["sentry-node-7", "sentry-node-10"] {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("e2e/fixtures")
            .join(dir);
        for entry in std::fs::read_dir(path).unwrap() {
            let bytes = std::fs::read(entry.unwrap().path()).unwrap();
            let Ok(envelope) = crate::envelope::Envelope::parse(&bytes) else {
                continue;
            };
            for item in envelope.items {
                if item.kind != crate::envelope::ItemKind::Event {
                    continue;
                }
                let mut raw: Value = serde_json::from_slice(item.payload).unwrap();
                raw["extra"] = json!({"password": "hidden"});
                Policy::default().value(&mut raw);
                let event = crate::event::ErrorEvent::parse(raw).unwrap();
                assert!(!event.raw.to_string().contains("hidden"));
                assert_ne!(crate::grouping::group(&event).fingerprint, "");
            }
        }
    }
}

#[test]
fn removed_values_do_not_split_issues() {
    let fingerprints: Vec<_> = ["abc", "xyz"]
        .iter()
        .map(|token| {
            let mut raw = json!({"message": format!("failed token={token}")});
            Policy::default().value(&mut raw);
            crate::grouping::group(&crate::event::ErrorEvent::parse(raw).unwrap()).fingerprint
        })
        .collect();
    assert_eq!(fingerprints[0], fingerprints[1]);
}

#[test]
fn urls_and_forms_do_not_hide_assignments_or_ip_addresses() {
    let mut text = "url=https://example.com?token=hidden&ok=1 ip=http://127.0.0.1:8080/".to_owned();
    Policy::default().text(&mut text);
    assert_eq!(
        text,
        "url=https://example.com?token=[Filtered: sensitive]&ok=1 ip=http://127.0.0.1:8080/"
    );
}

#[test]
fn authorization_text_does_not_leave_the_credential_after_the_scheme() {
    let mut text = "Authorization: Bearer abcdefghijk".to_owned();
    Policy::default().text(&mut text);
    assert_eq!(text, "Authorization=[Filtered: sensitive]");
}

#[test]
fn adjacent_cards_cannot_hide_each_other() {
    let mut text = "4242424242424242 4242424242424242".to_owned();
    Policy::default().text(&mut text);
    assert_eq!(text, "[Filtered: card] [Filtered: card]");
}

#[test]
fn all_classes_can_be_disabled() {
    let policy: Policy = serde_json::from_value(json!({"keys":false,"credentials":false,
        "cards":false,"email":false,"ip":false,"personal":false}))
    .unwrap();
    let original = json!({"password":"visible", "email":"a@example.com", "name":"Ada",
        "message":"Bearer abc 127.0.0.1 4242424242424242"});
    let mut value = original.clone();
    policy.value(&mut value);
    assert_eq!(value, original);
}

#[test]
fn opaque_request_bodies_and_attachments_are_removed_in_full() {
    let mut value = json!({"request":{"data":"multipart raw secret"},
        "extra":{"attachment":{"content":"opaque encoded secret"}}});
    Policy::default().value(&mut value);
    assert_eq!(
        value,
        json!({"request":{"data":"[Filtered: request body]"},
        "extra":{"attachment":{"content":"[Filtered: sensitive]"}}})
    );
}

#[test]
fn user_identity_fields_are_removed_without_touching_event_ids() {
    let mut value = json!({"event_id":"abc", "user":{"id":123,"name":"Ada","username":"ada"}});
    Policy {
        personal: true,
        ..Policy::default()
    }
    .value(&mut value);
    assert_eq!(
        value,
        json!({"event_id":"abc","user":{"id":123,
        "name":"[Filtered: personal]","username":"[Filtered: personal]"}})
    );
}

#[test]
fn numeric_protocol_ids_are_not_mistaken_for_adjacent_cards() {
    let mut value = json!({"event_id":"22222222222222222222222222222222",
        "contexts":{"trace":{"trace_id":"22222222222222222222222222222222"}}});
    let original = value.clone();
    Policy::default().value(&mut value);
    assert_eq!(value, original);
}

#[test]
fn fractional_durations_are_not_card_numbers() {
    let mut value = json!({"duration":0.424_242_424_242_424_2, "timestamp":1_800_000_000.424_242});
    let original = value.clone();
    Policy::default().value(&mut value);
    assert_eq!(value, original);
}

#[test]
fn normal_events_keep_code_metadata_identity_and_timestamps() {
    let mut value = json!({
        "frames":[{"function":"Cache::get"},{"function":"std::fmt::write"},{"function":"Foo::Bar"}],
        "contexts":{"os":{"name":"Linux"},"browser":{"name":"Chrome"},"runtime":{"name":"Rust"}},
        "sdk":{"name":"sentry.rust"}, "user":{"id":"123","email":"ana@example.com","name":"Ana"},
        "extra":{"author":"Ada","className":"Button","max_tokens":100,"authenticated":true},
        "timestamps":[1_732_441_234_561_u64,1_732_441_234_561_000_u64,1_732_441_234_561_000_000_u64],
        "message":"assembly 4.0.0.0 Cache::get std::fmt::write Foo::Bar"
    });
    Policy::default().value(&mut value);
    assert_eq!(
        value,
        json!({
            "frames":[{"function":"Cache::get"},{"function":"std::fmt::write"},{"function":"Foo::Bar"}],
            "contexts":{"os":{"name":"Linux"},"browser":{"name":"Chrome"},"runtime":{"name":"Rust"}},
            "sdk":{"name":"sentry.rust"}, "user":{"id":"123","email":"ana@example.com","name":"Ana"},
            "extra":{"author":"Ada","className":"Button","max_tokens":100,"authenticated":true},
            "timestamps":[1_732_441_234_561_u64,1_732_441_234_561_000_u64,1_732_441_234_561_000_000_u64],
            "message":"assembly [Filtered: ip] Cache::get std::fmt::write Foo::Bar"
        })
    );
}

#[test]
fn opt_in_identity_is_scoped_and_email_covers_text() {
    let policy = Policy {
        personal: true,
        email: true,
        ..Policy::default()
    };
    let mut value = json!({"user":{"id":"123","username":"ana","name":"Ana","email":"ana@example.com"},
        "contexts":{"os":{"name":"Linux"},"browser":{"name":"Chrome"},"runtime":{"name":"Rust"},
        "feedback":{"name":"Ana","contact_email":"ana@example.com"}},"sdk":{"name":"sentry.rust"},
        "name":"event", "message":"mail ana@example.com"});
    policy.value(&mut value);
    assert_eq!(
        value,
        json!({"user":{"id":"[Filtered: personal]","username":"[Filtered: personal]",
        "name":"[Filtered: personal]","email":"[Filtered: email]"},
        "contexts":{"os":{"name":"Linux"},"browser":{"name":"Chrome"},"runtime":{"name":"Rust"},
        "feedback":{"name":"[Filtered: personal]","contact_email":"[Filtered: email]"}},
        "sdk":{"name":"sentry.rust"},"name":"event","message":"mail [Filtered: email]"})
    );
    let mut feedback = json!({"name":"Ana","email":"ana@example.com","comments":"hello"});
    policy.item(&mut feedback, Item::Feedback, &mut Meter::unmetered());
    assert_eq!(
        feedback,
        json!({"name":"[Filtered: personal]","email":"[Filtered: email]","comments":"hello"})
    );
}

#[test]
fn sensitive_words_recurse_but_keep_non_string_values() {
    let mut value = json!({"Authorization":"Basic abc","cookies":"sid=abc","x-auth-token":"abc",
        "PHPSESSID":"abc","access_token":"abc","authtoken":"abc","csrftoken":"abc","oauth":"abc",
        "apiKey":"abc","privateKey":"abc","password":{"text":"abc","array":["abc",7,true,null]},
        "token":42,"auth":false,"cvv":null,"card_number":4_242_424_242_424_242_u64,
        "author":"Ada","className":"Button","tokens":"count","max_tokens":100,"authenticated":true});
    Policy::default().value(&mut value);
    assert_eq!(
        value,
        json!({"Authorization":"[Filtered: sensitive]","cookies":"[Filtered: sensitive]",
        "x-auth-token":"[Filtered: sensitive]","PHPSESSID":"[Filtered: sensitive]",
        "access_token":"[Filtered: sensitive]","authtoken":"[Filtered: sensitive]",
        "csrftoken":"[Filtered: sensitive]","oauth":"[Filtered: sensitive]",
        "apiKey":"[Filtered: sensitive]","privateKey":"[Filtered: sensitive]",
        "password":{"text":"[Filtered: sensitive]","array":["[Filtered: sensitive]",7,true,null]},
        "token":42,"auth":false,"cvv":null,"card_number":"[Filtered: card]",
        "author":"Ada","className":"Button","tokens":"count","max_tokens":100,"authenticated":true})
    );
}

#[test]
fn standalone_ips_skip_loopback_unspecified_and_embedded_candidates() {
    let mut value = json!({"message":"from 192.0.2.1 to [2001:db8::1] 127.0.0.1 ::1 0.0.0.0 ::",
        "code":"Cache::get std::fmt::write Foo::Bar v4.0.0.0 4.0.0.0suffix _4.0.0.0 4.0.0.0:80 1.2.3.4.5"});
    Policy::default().value(&mut value);
    assert_eq!(
        value,
        json!({"message":"from [Filtered: ip] to [[Filtered: ip]] 127.0.0.1 ::1 0.0.0.0 ::",
        "code":"Cache::get std::fmt::write Foo::Bar v4.0.0.0 4.0.0.0suffix _4.0.0.0 [Filtered: ip] 1.2.3.4.5"})
    );
}

#[test]
fn issuer_cards_and_query_assignments_still_match() {
    let mut value = json!({"message":"cards 4242424242424242 5555-5555-5555-4444 378282246310005",
        "url":"https://example.com/?api_key=abc&max_tokens=100&author=Ada",
        "number":5_555_555_555_554_444_u64,"timestamp":1_732_441_234_561_u64});
    Policy::default().value(&mut value);
    assert_eq!(
        value,
        json!({"message":"cards [Filtered: card] [Filtered: card] [Filtered: card]",
        "url":"https://example.com/?api_key=[Filtered: sensitive]&max_tokens=100&author=Ada",
        "number":"[Filtered: card]","timestamp":1_732_441_234_561_u64})
    );
}

#[test]
fn scrubs_ips_with_ports_and_at_sentence_ends() {
    let mut text = concat!(
        "connect ECONNREFUSED 10.0.0.5:5432, peer [2001:db8::7]:443, ",
        "client 203.0.113.9. Kept: http://127.0.0.1:8080/ v1.2.3.4.5 Foo::Bar 10:30:00"
    )
    .to_owned();
    Policy::default().text(&mut text);
    assert_eq!(
        text,
        concat!(
            "connect ECONNREFUSED [Filtered: ip], peer [[Filtered: ip]]:443, ",
            "client [Filtered: ip]. Kept: http://127.0.0.1:8080/ v1.2.3.4.5 Foo::Bar 10:30:00"
        )
    );
}
