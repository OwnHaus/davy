//! CPU cost of parsing and grouping a captured SDK event with and without
//! scrubbing, and of scrubbing hostile input alone.
use anyhow::Result;
use davy::{envelope::Envelope, event::ErrorEvent, grouping, scrub::Policy};
use serde_json::{Value, json};
use std::{hint::black_box, io::Write, time::Instant};

const MIB: usize = 1024 * 1024;

fn run(bytes: &[u8], count: u32, scrub: bool) -> Result<f64> {
    let policy = Policy::default();
    let start = Instant::now();
    for _ in 0..count {
        let mut raw: Value = serde_json::from_slice(black_box(bytes))?;
        if scrub {
            policy.value(&mut raw);
        }
        let event = ErrorEvent::parse(raw).map_err(anyhow::Error::msg)?;

        black_box(grouping::group(black_box(&event)));
    }
    Ok(start.elapsed().as_secs_f64() * 1e6 / f64::from(count))
}

fn captured(out: &mut impl Write) -> Result<()> {
    let body = include_bytes!("../e2e/fixtures/sentry-node-10/4.body");
    let envelope = Envelope::parse(body).map_err(anyhow::Error::msg)?;
    let mut raw: Value = serde_json::from_slice(envelope.items[0].payload)?;
    raw["extra"] = json!({"password":"bench-secret", "note":"mail a@example.com"});
    // Warm the fixed regex automata before timing steady-state ingest.
    let mut warm = raw.clone();
    Policy::default().value(&mut warm);
    anyhow::ensure!(!warm.to_string().contains("bench-secret"));
    for (label, count) in [("realistic", 100), ("large", 10)] {
        if label == "large" {
            raw["extra"]["padding"] = "ordinary text and token=bench-secret & "
                .repeat(24_000)
                .into();
        }
        let mut checked = raw.clone();
        Policy::default().value(&mut checked);
        anyhow::ensure!(!checked.to_string().contains("bench-secret"));
        let bytes = serde_json::to_vec(&raw)?;
        writeln!(
            out,
            "{label}: {} bytes; {count} events/sample; us/event",
            bytes.len()
        )?;
        for _ in 0..5 {
            let baseline = run(&bytes, count, false)?;
            let scrubbed = run(&bytes, count, true)?;
            writeln!(
                out,
                "baseline {baseline:.2}, scrubbed {scrubbed:.2}, delta {:.2}",
                scrubbed - baseline
            )?;
        }
    }
    Ok(())
}

/// The fastest of five runs of `policy` over a fresh copy of `value`, in
/// milliseconds, and the size of the scrubbed JSON.
fn scrub_ms(policy: &Policy, value: &Value) -> (f64, usize) {
    let mut best = f64::MAX;
    let mut size = 0;
    for _ in 0..5 {
        let mut copy = value.clone();
        let start = Instant::now();
        policy.value(black_box(&mut copy));
        best = best.min(start.elapsed().as_secs_f64() * 1e3);
        size = copy.to_string().len();
    }
    (best, size)
}

fn policies() -> Result<Vec<(&'static str, Policy)>> {
    let long: Vec<String> = (0..100)
        .map(|i| format!("{i:03}{}", "x".repeat(97)))
        .collect();
    Ok(vec![
        ("defaults", Policy::default()),
        (
            "100 extra keys",
            serde_json::from_value(json!({"extra_keys": long}))?,
        ),
        ("email on", serde_json::from_value(json!({"email": true}))?),
    ])
}

/// Shapes a review found slow, and repeated strings that make each pattern
/// match as often as it can, each about 1 MiB.
fn hostile() -> Vec<(String, Value)> {
    let keys: serde_json::Map<String, Value> =
        (0..60_000).map(|i| (format!("k{i}"), json!(1))).collect();
    let addresses: serde_json::Map<String, Value> = (0..60_000)
        .map(|i| {
            (
                format!("10.{}.{}.{}", i >> 16, (i >> 8) & 255, i & 255),
                json!(1),
            )
        })
        .collect();
    // U+FDFA decomposes to 18 letters, the widest fold there is.
    let wide: serde_json::Map<String, Value> = (0..4_000)
        .map(|i| (format!("{}{i:05}", "\u{fdfa}".repeat(83)), json!(1)))
        .collect();
    let lookalike: serde_json::Map<String, Value> = (0..60_000)
        .map(|i| (format!("\u{43e}\u{fdfa}{i}"), json!(1)))
        .collect();
    let mut cases = vec![
        ("60k-key object".to_owned(), Value::Object(keys)),
        ("4k 254-byte U+FDFA keys".to_owned(), Value::Object(wide)),
        ("60k non-ASCII keys".to_owned(), Value::Object(lookalike)),
        ("60k IP keys, renamed".to_owned(), Value::Object(addresses)),
        (
            "100k [\"k\",1] pairs".to_owned(),
            Value::Array(vec![json!(["k", 1]); 100_000]),
        ),
    ];
    for unit in [
        "a:",
        "a=",
        "pwd:",
        "a:b ",
        ":: ",
        "1:: ",
        "a@b.c ",
        "1.1.1.1 ",
        "4242424242424242 ",
        "bearer x ",
        "ghp_x ",
        "aaaa",
        "pwd=x&",
        "\\\"k\\\":\\\"v\\\",",
        "\u{43e}=",
        "\u{fdfa}:",
        "%25%25%25=",
        "p%252561ssword=x&",
    ] {
        let text = unit.repeat(MIB / unit.len());
        cases.push((format!("1 MiB of {unit:?}"), json!({ "message": text })));
    }
    cases
}

fn hostile_costs(out: &mut impl Write) -> Result<()> {
    writeln!(
        out,
        "hostile: best of 5, ms; scrubbed JSON size / input size"
    )?;
    let policies = policies()?;
    for (label, value) in hostile() {
        let input = value.to_string().len();
        for (name, policy) in &policies {
            let (ms, size) = scrub_ms(policy, &value);
            #[expect(
                clippy::cast_precision_loss,
                reason = "sizes stay far below 2^52 bytes"
            )]
            let growth = size as f64 / input as f64;
            writeln!(out, "{label}, {name}: {ms:.1} ms, x{growth:.2}")?;
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    let mut out = std::io::stdout().lock();
    if std::env::args().all(|a| a != "--hostile-only") {
        captured(&mut out)?;
    }
    hostile_costs(&mut out)
}
