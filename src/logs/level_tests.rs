use super::*;
use serde_json::json;

#[test]
fn level_table() {
    use LogLevel::*;
    for (text, level) in [
        ("trace", Trace),
        ("DEBUG", Debug),
        ("info", Info),
        ("information", Info),
        ("notice", Info),
        ("log", Info),
        ("warn", Warn),
        ("Warning", Warn),
        ("error", Error),
        ("err", Error),
        ("fatal", Fatal),
        ("critical", Fatal),
        ("crit", Fatal),
        ("alert", Fatal),
        ("emerg", Fatal),
        ("emergency", Fatal),
        ("panic", Fatal),
    ] {
        assert_eq!(LogLevel::parse(text), Some(level), "{text}");
    }
    assert_eq!(LogLevel::parse("loud"), None);
    for (n, level) in [
        (10.0, Trace),
        (20.0, Debug),
        (30.0, Info),
        (40.0, Warn),
        (50.0, Error),
        (60.0, Fatal),
        (35.0, Warn),
    ] {
        assert_eq!(LogLevel::from_pino(n), level, "{n}");
    }
    for (n, level) in [
        (1, Trace),
        (4, Trace),
        (5, Debug),
        (8, Debug),
        (9, Info),
        (12, Info),
        (13, Warn),
        (16, Warn),
        (17, Error),
        (20, Error),
        (21, Fatal),
        (24, Fatal),
    ] {
        assert_eq!(LogLevel::from_severity(n, ""), Some(level), "{n}");
    }
    assert_eq!(LogLevel::from_severity(0, "WARN"), Some(Warn));
    assert_eq!(LogLevel::from_severity(0, "?"), None);
    assert_eq!(LogLevel::from_value(Some(&json!(true))), None);
    assert_eq!(LogLevel::from_value(None), None);
}

#[test]
fn the_common_spellings_of_a_level_are_read() {
    use LogLevel::*;
    for (text, level) in [
        ("severe", Error),
        ("FAULT", Error),
        ("failure", Error),
        ("Exception", Error),
        ("T", Trace),
        ("d", Debug),
        ("I", Info),
        ("W", Warn),
        ("E", Error),
        ("F", Fatal),
        ("[error]", Error),
        ("error:", Error),
        (" <warn> ", Warn),
        ("Level.ERROR", Error),
        ("log.level.debug", Debug),
    ] {
        assert_eq!(LogLevel::parse(text), Some(level), "{text}");
    }
    for text in ["loud", "", "err0r", "x", "[]", "1.5", "Level."] {
        assert_eq!(LogLevel::parse(text), None, "{text}");
    }
    for (v, level) in [
        (json!("40"), Some(Warn)),
        (json!(" 50 "), Some(Error)),
        (json!("30.0"), Some(Info)),
        (json!("NaN"), None),
        (json!("inf"), None),
        (json!(""), None),
        (json!(null), None),
    ] {
        assert_eq!(LogLevel::from_value(Some(&v)), level, "{v}");
    }
}
