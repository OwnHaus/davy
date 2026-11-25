use super::*;
use serde_json::Value;

const REPORT: &str = include_str!("../../bench/footprint.json");

/// `value` times 10^6, as a whole number of parts per million.
fn ppm(value: &Value) -> u64 {
    let f = value.as_f64().unwrap();
    // A coefficient of 4 decimals times 10^6 is a whole number.
    let n = (f * 1_000_000.0).round();
    assert!(
        (n - f * 1_000_000.0).abs() < 1e-3,
        "{f} has more than 6 decimals"
    );
    format!("{n:.0}").parse().unwrap()
}

#[test]
fn footprint_constants_match_the_bench_report() {
    let r: Value = serde_json::from_str(REPORT).unwrap();
    assert_eq!(
        (&r["schema"], &r["version"]),
        (&Value::from("davy.footprint"), &Value::from(1)),
        "a new schema or version of the report needs the constants read again"
    );
    let logs = &r["coefficients"]["logs"];
    let ratio = &logs["disk_per_raw_real"];
    assert_eq!(
        PRIOR.ratio,
        Band {
            low: ppm(&ratio["min"]),
            mid: ppm(&ratio["median"]),
            high: ppm(&ratio["max"]),
        }
    );
    assert_eq!(
        PRIOR.day_index,
        logs["extra_bytes_per_day_index"]["median"]
            .as_u64()
            .unwrap()
    );
    let m = &r["machine"];
    let source = format!(
        "bench/footprint.json, measured {} at {} on {} ({})",
        r["measured"].as_str().unwrap(),
        r["davy_commit"].as_str().unwrap(),
        m["cpu"].as_str().unwrap(),
        m["model"].as_str().unwrap()
    );
    assert_eq!(PRIOR.source, source);
    let Band { low, mid, high } = PRIOR.ratio;
    for (name, n) in [
        ("low", low),
        ("mid", mid),
        ("high", high),
        ("day_index", PRIOR.day_index),
    ] {
        assert_ne!(n, 0, "{name} is 0: fill it from the report");
    }
    assert!(low <= mid && mid <= high);
}
