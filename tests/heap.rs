//! The heap estimates are upper bounds on what parsing allocates, measured
//! by an allocator that tracks the peak.

use davy::heap;
use peak_alloc::PeakAlloc;
use serde_json::Value;

#[global_allocator]
static ALLOC: PeakAlloc = PeakAlloc;

/// The most memory `parse` held at once beyond what was held before it.
fn peak<T>(parse: impl FnOnce() -> T) -> usize {
    ALLOC.reset_peak_usage();
    let before = ALLOC.current_usage();
    let out = parse();
    let peak = ALLOC.peak_usage() - before;
    drop(out);
    peak
}

fn scrambled_keys(n: usize) -> String {
    let fields: Vec<String> = (0..n)
        .map(|i| format!("\"k{}\":{i}", i.wrapping_mul(7919) % n))
        .collect();
    format!("{{{}}}", fields.join(","))
}

fn json_bodies() -> Vec<(&'static str, String)> {
    let n = 100_000;
    vec![
        ("empty objects", format!("[{}{{}}]", "{},".repeat(n))),
        ("zeros", format!("[{}0]", "0,".repeat(n))),
        ("empty arrays", format!("[{}[]]", "[],".repeat(n))),
        ("short strings", format!("[{}\"\"]", "\"a\",".repeat(n))),
        (
            "ascending keys",
            format!(
                "{{{}}}",
                (0..n)
                    .map(|i| format!("\"{i:07}\":0"))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        ),
        ("scrambled keys", scrambled_keys(n)),
        (
            "one-key objects",
            format!("[{}{{}}]", "{\"a\":1},".repeat(n)),
        ),
        ("deep", format!("{}{}", "[".repeat(127), "]".repeat(127))),
    ]
}

#[test]
fn estimates_bound_the_peak_heap_of_parsing() {
    for (name, body) in json_bodies() {
        let body = body.as_bytes();
        let estimate = heap::json(body, 0).unwrap();
        let used = peak(|| serde_json::from_slice::<Value>(body).unwrap());
        assert!(
            used <= estimate,
            "{name} json: used {used}, estimated {estimate}"
        );
    }
}
