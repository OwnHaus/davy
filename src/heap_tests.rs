use super::*;
use crate::logs::{LogLevel, LogSource};

// The literal prices below hold on 64-bit targets: a Value is 32 bytes and a
// String 24, so VALUE is 96, ARRAY 128, ENTRY 168 and MAP 632.

#[test]
fn prices_follow_the_layout_of_the_parsed_types() {
    assert_eq!((VALUE, ARRAY, ENTRY, MAP), (96, 128, 168, 632));
}

#[test]
fn empty_objects_pay_the_per_object_price() {
    assert_eq!(json(b"[{},{}]", 100).unwrap(), 96 + 128 + 2 * (96 + 100));
    assert_eq!(json(b"{}", 0).unwrap(), 96);
}

#[test]
fn an_object_pays_for_its_first_node_its_entries_and_its_text() {
    let body = br#"{"a":"bcd"}"#;
    assert_eq!(json(body, 0).unwrap(), 96 + 632 + 168 + 16 + 16);
    assert_eq!(of_value(&serde_json::from_slice(body).unwrap()), 928);
}

#[test]
fn the_count_matches_the_tree_it_stands_for() {
    let bodies: [&[u8]; 5] = [
        br#"[1, "two", null, true, 4.5]"#,
        br#"{"nested": {"deeper": [{"k": "v"}, [], {}]}, "list": ["a", "bb"]}"#,
        br#""a string of more than sixteen bytes""#,
        br#"{"escaped \" key": "tab\tand \u00e9"}"#,
        br"[[[[[]]]]]",
    ];
    for body in bodies {
        let tree = serde_json::from_slice::<Value>(body).unwrap();
        assert_eq!(json(body, 0).unwrap(), of_value(&tree), "{body:?}");
    }
}

#[test]
fn a_repeated_key_is_counted_each_time() {
    let body = br#"{"a":1,"a":2}"#;
    let tree = serde_json::from_slice::<Value>(body).unwrap();
    assert_eq!(json(body, 0).unwrap(), of_value(&tree) + 168 + 16);
}

#[test]
fn the_count_fails_where_parsing_fails() {
    let deep = format!("{}{}", "[".repeat(129), "]".repeat(129));
    for body in [&b"[1,2"[..], b"{} x", b"", deep.as_bytes()] {
        assert!(json(body, 0).is_err(), "{body:?}");
        assert!(serde_json::from_slice::<Value>(body).is_err());
    }
}

#[test]
fn ndjson_counts_each_line_and_skips_blank_ones() {
    assert_eq!(ndjson(b"{}\n \n{}\r\n", 10).unwrap(), 2 * (96 + 10));
    assert!(ndjson(b"{}\n{", 0).is_err());
}

#[test]
fn a_record_pays_for_its_text_and_attributes() {
    let record = LogRecord {
        project_id: 1,
        timestamp: 0,
        observed_at: 0,
        source: LogSource::Logtail,
        level: LogLevel::Info,
        message: "hello".into(),
        service: Some("api".into()),
        trace_id: None,
        span_id: None,
        attributes: serde_json::json!({"k": "v"}).as_object().unwrap().clone(),
    };
    assert_eq!(of_record(&record), RECORD + 16 + 16 + 632 + 168 + 16 + 16);
}

#[test]
fn a_messagepack_tree_is_held_beside_its_json() {
    let scale = 1 + MSGPACK_SCALE;
    // {"a": "bcd"}
    let body = [0x81, 0xa1, b'a', 0xa3, b'b', b'c', b'd'];
    assert_eq!(msgpack(&body, 0), Some(928 * scale));
    // {1: 2} prints its key as text.
    assert_eq!(
        msgpack(&[0x81, 0x01, 0x02], 0),
        Some((96 + 632 + 168 + 16) * scale)
    );
    // [{}, {}, {}]
    let body = [0x93, 0x80, 0x80, 0x80];
    assert_eq!(
        msgpack(&body, 100),
        Some((96 + 128 + 3 * (96 + 100)) * scale)
    );
    // bin 8 of three bytes becomes base64 text.
    assert_eq!(msgpack(&[0xc4, 3, 1, 2, 3], 0), Some((96 + 16) * scale));
}

#[test]
fn a_messagepack_value_outweighs_a_json_value_at_most_twice() {
    assert!(size_of::<rmpv::Value>() <= 2 * size_of::<Value>());
    assert_eq!(MSGPACK_SCALE, size_of::<rmpv::Value>().div_ceil(32));
}

#[test]
fn broken_messagepack_has_no_price() {
    let deep = [vec![0x91; 130], vec![0xc0]].concat();
    let cases: [&[u8]; 6] = [
        &[],
        &[0xc1],
        &[0x92, 0x01],
        &[0xa3, b'a'],
        &[0x01, 0x02],
        &deep,
    ];
    for body in cases {
        assert_eq!(msgpack(body, 0), None, "{body:?}");
    }
    let huge = [0xdd, 0xff, 0xff, 0xff, 0xff, 0x80];
    assert_eq!(msgpack(&huge, 0), None, "a count the body cannot hold");
}
