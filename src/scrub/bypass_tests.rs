use super::*;
use serde_json::json;

fn text(policy: &Policy, input: &str) -> String {
    let mut text = input.to_owned();
    policy.text(&mut text);
    text
}

#[test]
fn name_value_pairs_hide_the_value_whatever_the_name_field_is_called() {
    let mut value = json!({"headers":[
        {"name":"Authorization","value":"Basic abc"},
        {"Name":"X-Api-Key","Value":"abc"},
        {"Key":"password","value":"abc"},
        {"key":"session","value":"abc"},
        {"name":"X-Request-Id","value":"abc"}]});
    Policy::default().value(&mut value);
    assert_eq!(
        value,
        json!({"headers":[
        {"name":"Authorization","value":"[Filtered: sensitive]"},
        {"Name":"X-Api-Key","Value":"[Filtered: sensitive]"},
        {"Key":"password","value":"[Filtered: sensitive]"},
        {"key":"session","value":"[Filtered: sensitive]"},
        {"name":"X-Request-Id","value":"abc"}]})
    );
}

#[test]
fn json_escaped_inside_a_string_does_not_hide_an_assignment() {
    assert_eq!(
        text(
            &Policy::default(),
            r#"body {\"password\":\"hun\\\"ter2\",\"user\":\"ada\"} sent"#
        ),
        r#"body {\"password=[Filtered: sensitive],\"user\":\"ada\"} sent"#
    );
    assert_eq!(
        text(&Policy::default(), r#"{\"api_key\": 12345}"#),
        r#"{\"api_key=[Filtered: sensitive]}"#
    );
}

#[test]
fn keys_that_hold_removed_values_are_renamed() {
    let policy = Policy {
        email: true,
        ..Policy::default()
    };
    let mut value = json!({"by_user":{"ana@example.com":1,"bob@example.com":2,"[Filtered: email]":0},
        "by_ip":{"192.0.2.1":{"hits":3},"127.0.0.1":4},
        "by_card":{"4242424242424242":5}, "by_token":{"ghp_abcdefghij":6}, "plain":{"k1":7}});
    policy.value(&mut value);
    assert_eq!(
        value,
        json!({"by_user":{"[Filtered: email]":0,"[Filtered: email] 2":1,"[Filtered: email] 3":2},
        "by_ip":{"[Filtered: ip]":{"hits":3},"127.0.0.1":4},
        "by_card":{"[Filtered: card]":5}, "by_token":{"[Filtered: credential]":6}, "plain":{"k1":7}})
    );
}

#[test]
fn a_renamed_key_still_hides_a_sensitive_value() {
    let mut value = json!({"password for 192.0.2.1":"hunter2"});
    Policy::default().value(&mut value);
    assert_eq!(value, json!({"[Filtered: ip]":"[Filtered: sensitive]"}));
}

#[test]
fn more_key_roots_are_sensitive_and_short_ones_must_be_whole_words() {
    let mut value = json!({"passphrase":"x","StorageAccountKey":"x","SharedAccessKey":"x",
        "x-hub-signature":"x","request_signature":"x","pin":"x","user_pin":"x","PIN":"x",
        "pinCode":"x","otp":"x","otp_code":"x","spinner":"x","shipping":"x","pinned":"x",
        "option":"x","topping":"x"});
    Policy::default().value(&mut value);
    let hidden = "[Filtered: sensitive]";
    assert_eq!(
        value,
        json!({"passphrase":hidden,"StorageAccountKey":hidden,"SharedAccessKey":hidden,
        "x-hub-signature":hidden,"request_signature":hidden,"pin":hidden,"user_pin":hidden,
        "PIN":hidden,"pinCode":hidden,"otp":hidden,"otp_code":hidden,"spinner":"x",
        "shipping":"x","pinned":"x","option":"x","topping":"x"})
    );
    assert_eq!(
        text(
            &Policy::default(),
            "DefaultEndpointsProtocol=https;AccountName=x;AccountKey=abc==;EndpointSuffix=core"
        ),
        "DefaultEndpointsProtocol=https;AccountName=x;AccountKey=[Filtered: sensitive];EndpointSuffix=core"
    );
}

#[test]
fn more_credential_formats_are_recognized() {
    for credential in [
        "AIzaSyD-abcdefghijklmnopqrstuvwxyz01234",
        "glpat-abcdefghij0123456789",
        "npm_abcdefghijklmnopqrstuvwxyz0123456789",
        "whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw",
        "SG.abcdefghijklmnop.abcdefghijklmnopqrstu",
        "eyJhbGciOiJub25lIn0.eyJzdWIiOiIxIn0",
        "eyJhbGciOiJub25lIn0.eyJzdWIiOiIxIn0.",
    ] {
        assert_eq!(
            text(&Policy::default(), &format!("got {credential} here")),
            "got [Filtered: credential] here",
            "{credential}"
        );
    }
    assert_eq!(
        text(&Policy::default(), "aiza glpat npm_ sg.a.b eyJ.x"),
        "aiza glpat npm_ sg.a.b eyJ.x"
    );
}

#[test]
fn too_many_sensitive_assignments_replace_the_whole_string() {
    let at_cap = "pwd=x&".repeat(patterns::MAX_ASSIGNMENTS);
    assert_eq!(
        text(&Policy::default(), &at_cap),
        "pwd=[Filtered: sensitive]&".repeat(patterns::MAX_ASSIGNMENTS)
    );
    let over = "pwd=x&".repeat(patterns::MAX_ASSIGNMENTS + 1);
    assert_eq!(
        text(&Policy::default(), &over),
        "[Filtered: too many assignments]"
    );
    let trace = "at Foo.bar (Foo.java:12) key=value\n".repeat(400);
    assert_eq!(text(&Policy::default(), &trace), trace);
}

#[test]
fn root_structural_fields_keep_their_values_but_not_sensitive_keys() {
    let mut value = json!({"release":"app@1.2.3.4","dist":"10.0.0.1","environment":"192.0.2.1",
        "platform":"node","level":"4242424242424242","logger":"a@b.co","event_id":"1.2.3.4",
        "contexts":{"trace":{"trace_id":"1.2.3.4","span_id":"1.2.3.4","parent_span_id":"1.2.3.4"}},
        "tags":[["release","1.2.3.4"],["server","1.2.3.4"]],
        "extra":{"version":"1.2.3.4","release":{"token":"hidden"}},
        "monitor_slug":"10.0.0.1","check_in_id":"10.0.0.1","did":"10.0.0.1"});
    Policy {
        email: true,
        ..Policy::default()
    }
    .value(&mut value);
    assert_eq!(
        value,
        json!({"release":"app@1.2.3.4","dist":"10.0.0.1","environment":"192.0.2.1",
        "platform":"node","level":"4242424242424242","logger":"a@b.co","event_id":"1.2.3.4",
        "contexts":{"trace":{"trace_id":"1.2.3.4","span_id":"1.2.3.4","parent_span_id":"1.2.3.4"}},
        "tags":[["release","[Filtered: ip]"],["server","[Filtered: ip]"]],
        "extra":{"version":"[Filtered: ip]","release":{"token":"[Filtered: sensitive]"}},
        "monitor_slug":"10.0.0.1","check_in_id":"10.0.0.1","did":"[Filtered: ip]"})
    );
}

#[test]
fn session_ids_count_users_and_are_kept() {
    let policy = Policy {
        email: true,
        ..Policy::default()
    };
    let session = json!({"sid":"10.0.0.1","did":"ana@example.com","status":"ok",
        "attrs":{"release":"1.2.3.4","environment":"prod","ip_address":"192.0.2.7"}});
    let mut value = session.clone();
    policy.item(&mut value, Item::Session, &mut Meter::unmetered());
    assert_eq!(
        value,
        json!({"sid":"10.0.0.1","did":"ana@example.com","status":"ok",
        "attrs":{"release":"1.2.3.4","environment":"prod","ip_address":"[Filtered: ip]"}})
    );
    let mut value = session;
    policy.value(&mut value);
    assert_eq!(value["did"], "[Filtered: email]");
}

#[test]
fn the_meter_pays_growth_in_whole_kib_and_gives_back_the_excess() {
    let mut payments = Vec::new();
    let mut pay = |bytes| {
        payments.push(bytes);
        true
    };
    let mut meter = Meter::new(&mut pay);
    let mut value = Value::from(vec![4_242_424_242_424_242_u64; 1000]);
    Policy::default().item(&mut value, Item::Event, &mut meter);
    let (grown, excess, refused) = (meter.grown(), meter.excess(), meter.refused());
    assert_eq!((grown, excess, refused), (16_000, 384, false));
    assert_eq!(payments.iter().sum::<usize>(), 16_384);
    assert!(payments.iter().all(|bytes| bytes % 1024 == 0));
    assert_eq!(value[999], "[Filtered: card]");
}

#[test]
fn a_refused_payment_stops_the_scrub() {
    let mut paid = 0;
    let mut pay = |bytes| {
        paid += bytes;
        paid <= 4096
    };
    let mut meter = Meter::new(&mut pay);
    let mut value = json!({"message": "ghp_x ".repeat(10_000)});
    Policy::default().item(&mut value, Item::Event, &mut meter);
    assert!(meter.refused());
    let mut shrinking = json!({"message": format!("pwd={}", "x".repeat(10_000))});
    let mut never = |_| false;
    let mut meter = Meter::new(&mut never);
    Policy::default().item(&mut shrinking, Item::Event, &mut meter);
    assert!(
        meter.refused(),
        "the rewrite is paid before the old text is freed"
    );
}

#[test]
fn the_documented_gaps_pass_unchanged() {
    for gap in [
        "https://ada:pa/ss@example.test/",
        "card 4242.4242.4242.4242",
        "api key=abc",
        "p%25252561ssword=abc",
        "password\u{ff1d}abc",
    ] {
        assert_eq!(text(&Policy::default(), gap), gap);
    }
}

#[test]
fn many_colliding_renames_count_on() {
    let mut keys: Map<String, Value> = (0..1000)
        .map(|i| (format!("10.0.{}.{}", i / 250, i % 250 + 1), Value::from(i)))
        .collect();
    keys.insert("[Filtered: ip] 3".to_owned(), Value::from("kept"));
    let mut value = Value::Object(keys);
    Policy::default().value(&mut value);
    let map = value.as_object().unwrap();
    let names: Vec<String> = std::iter::once("[Filtered: ip]".to_owned())
        .chain((2..=1001).map(|n| format!("[Filtered: ip] {n}")))
        .collect();
    let mut expected: Vec<&String> = names.iter().collect();
    expected.sort_unstable();
    assert_eq!(map.keys().collect::<Vec<_>>(), expected);
    assert_eq!(map["[Filtered: ip] 3"], "kept");
    let mut numbers: Vec<u64> = map.values().filter_map(Value::as_u64).collect();
    numbers.sort_unstable();
    assert_eq!(numbers, (0..1000).collect::<Vec<_>>());
}
