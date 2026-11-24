//! The synthetic pino-shaped log row that `load_logs` and `footprint` send:
//! 70% request logs, 20% info, 7% warn, 3% errors with a stack trace.

use anyhow::{Context, Result};
use serde_json::{Value, json};

pub struct Rng(u64);
impl Rng {
    /// The seed every benchmark uses, so runs send the same rows.
    pub fn seeded() -> Self {
        Self(0x9e37_79b9_7f4a_7c15)
    }
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        let i = self.next() % items.len() as u64;
        items[usize::try_from(i).expect("an index below a slice length fits usize")]
    }
    fn hex(&mut self, len: usize) -> String {
        (0..len)
            .map(|_| char::from(self.pick(b"0123456789abcdef")))
            .collect()
    }
}

const ROUTES: [&str; 6] = [
    "/v1/events",
    "/v1/orders",
    "/v1/cart",
    "/health",
    "/v1/users/me",
    "/v1/search",
];
const SERVICES: [&str; 4] = ["api", "worker", "checkout", "search"];

/// Row `i` of a run that starts at `start_ms`, 50 rows per millisecond.
pub fn row(rng: &mut Rng, i: i64, start_ms: i64) -> Result<Value> {
    let dt = format!(
        "{:.3}",
        jiff::Timestamp::from_millisecond(start_ms + i / 50).context("timestamp out of range")?
    );
    let mut m = json!({
        "dt": dt,
        "pid": 4000 + rng.below(8),
        "hostname": format!("api-7d9f-{}", rng.hex(4)),
        "service": rng.pick(&SERVICES),
    });
    let roll = rng.below(100);
    let extra = if roll < 70 {
        let route = rng.pick(&ROUTES);
        let status = rng.pick(&[200, 200, 200, 201, 204, 304, 404, 500]);
        let method = rng.pick(&["GET", "POST"]);
        json!({
            "level": "info",
            "message": "request completed",
            "req": {"id": format!("r-{}", rng.hex(8)), "method": method, "url": format!("{route}/{}", rng.below(100_000))},
            "res": {"statusCode": status},
            "responseTime": rng.below(900),
            "trace_id": rng.hex(32),
        })
    } else if roll < 90 {
        json!({"level": "info", "message": format!("cache refreshed for tenant t-{}", rng.below(500)), "user": format!("u-{}", rng.below(50_000))})
    } else if roll < 97 {
        let ms = 1000 + rng.below(9000);
        json!({"level": "warn", "message": format!("slow query on orders took {ms}ms"), "ms": ms})
    } else {
        json!({"level": "error", "message": "database timeout", "err": {
            "type": "Error",
            "message": "connect ETIMEDOUT 10.0.1.5:5432",
            "stack": format!("Error: connect ETIMEDOUT 10.0.1.5:5432\n    at TCPConnectWrap.afterConnect (node:net:1611:16)\n    at Pool.connect (/app/node_modules/pg-pool/index.js:{}:9)", rng.below(400)),
        }})
    };
    if let (Value::Object(m), Value::Object(extra)) = (&mut m, extra) {
        m.extend(extra);
    }
    Ok(m)
}
