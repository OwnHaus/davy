use super::strip;
use crate::logs::{logtail, otlp, sentry};
use serde_json::json;

const COLOURED: &str = "\u{1b}[2m2026-11-23T09:28:07Z\u{1b}[0m \u{1b}[32m INFO\u{1b}[0m \u{1b}[2mmy_app\u{1b}[0m\u{1b}[2m:\u{1b}[0m connected";

#[test]
fn complete_csi_and_osc_sequences_go_and_everything_else_stays() {
    let cases = [
        (COLOURED, "2026-11-23T09:28:07Z  INFO my_app: connected"),
        ("\u{1b}[38;5;196mred\u{1b}[m", "red"),
        ("\u{1b}[?25lhidden cursor", "hidden cursor"),
        ("\u{1b}[1 qshape", "shape"),
        ("\u{1b}]0;build 7\u{7}done", "done"),
        (
            "see \u{1b}]8;;https://example.com\u{1b}\\the docs\u{1b}]8;;\u{1b}\\ now",
            "see the docs now",
        ),
        ("é\u{1b}]2;tíτλε\u{7}ü", "éü"),
        ("cut \u{1b}[3", "cut \u{1b}[3"),
        ("cut \u{1b}[", "cut \u{1b}["),
        ("cut \u{1b}]0;tit", "cut \u{1b}]0;tit"),
        ("\u{1b}[3\nm", "\u{1b}[3\nm"),
        ("\u{1b}[ 1m", "\u{1b}[ 1m"),
        ("\u{1b}]0;a\u{1b}[1mb", "\u{1b}]0;ab"),
        ("\u{1b}]0;a\nb\u{7}", "\u{1b}]0;a\nb\u{7}"),
        ("a\u{1b}b\tc\u{7}\u{1b}", "a\u{1b}b\tc\u{7}\u{1b}"),
        ("plain", "plain"),
    ];
    for (input, want) in cases {
        let mut s = input.to_owned();
        strip(&mut s);
        assert_eq!(s, want, "{input:?}");
    }
}

#[test]
fn every_adapter_strips_the_message_and_string_attributes() {
    let message = serde_json::to_string(COLOURED).unwrap();
    let logtail = format!(
        r#"{{"message":{message},"target":"\u001b[2mmy_app\u001b[0m","req":{{"path":["\u001b[1m/a\u001b[0m"]}}}}"#
    );
    let sentry = format!(
        r#"{{"items":[{{"body":{message},"attributes":{{"target":{{"value":"\u001b[2mmy_app\u001b[0m","type":"string"}}}}}}]}}"#
    );
    let otlp = format!(
        r#"{{"resourceLogs":[{{"scopeLogs":[{{"logRecords":[{{"body":{{"stringValue":{message}}},"attributes":[{{"key":"target","value":{{"stringValue":"\u001b[2mmy_app\u001b[0m"}}}}]}}]}}]}}]}}"#
    );
    let records = [
        logtail::parse(logtail.as_bytes(), "application/json", 1, 7).unwrap(),
        sentry::parse(sentry.as_bytes(), 1, 7).unwrap(),
        otlp::parse(otlp.as_bytes(), otlp::Encoding::Json, 1, 7).unwrap(),
    ];
    for r in &records {
        assert_eq!(r[0].message, "2026-11-23T09:28:07Z  INFO my_app: connected");
        assert_eq!(r[0].attributes.get("target"), Some(&json!("my_app")));
    }
    assert_eq!(
        records[0][0].attributes.get("req"),
        Some(&json!({"path": ["/a"]}))
    );
}

#[test]
fn a_message_that_is_not_text_is_stripped_before_it_is_printed() {
    let otlp = r#"{"resourceLogs":[{"scopeLogs":[{"logRecords":[{"body":{"kvlistValue":{"values":[{"key":"msg","value":{"stringValue":"\u001b[31mfailed\u001b[0m"}}]}}}]}]}]}"#;
    let records = otlp::parse(otlp.as_bytes(), otlp::Encoding::Json, 1, 7).unwrap();
    assert_eq!(records[0].message, r#"{"msg":"failed"}"#);
}
