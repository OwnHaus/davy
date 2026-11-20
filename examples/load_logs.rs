//! Sends synthetic pino-shaped logs to a running `davy serve` over the Logtail wire
//! format (gzip msgpack, batches of `--batch`) and reports the accept rate.
//!
//! cargo run --release --example load_logs -- 127.0.0.1:PORT KEY 1000000 1000 4

use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::TcpStream,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn hex(&mut self, len: usize) -> String {
        (0..len)
            .map(|_| char::from(b"0123456789abcdef"[self.below(16) as usize]))
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

fn row(rng: &mut Rng, i: usize, start_ms: i64) -> Value {
    let dt = chrono::DateTime::from_timestamp_millis(start_ms + i as i64 / 50)
        .unwrap()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let mut m = json!({
        "dt": dt,
        "pid": 4000 + rng.below(8),
        "hostname": format!("api-7d9f-{}", rng.hex(4)),
        "service": SERVICES[rng.below(4) as usize],
    });
    let roll = rng.below(100);
    let extra = if roll < 70 {
        let route = ROUTES[rng.below(6) as usize];
        let status = [200, 200, 200, 201, 204, 304, 404, 500][rng.below(8) as usize];
        let method = ["GET", "POST"][rng.below(2) as usize];
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
    m
}

fn msgpack(v: &Value) -> rmpv::Value {
    use rmpv::Value as M;
    match v {
        Value::Null => M::Nil,
        Value::Bool(b) => M::Boolean(*b),
        Value::Number(n) => n
            .as_i64()
            .map(M::from)
            .unwrap_or_else(|| M::F64(n.as_f64().unwrap_or(0.0))),
        Value::String(s) => M::from(s.as_str()),
        Value::Array(a) => M::Array(a.iter().map(msgpack).collect()),
        Value::Object(o) => M::Map(
            o.iter()
                .map(|(k, v)| (M::from(k.as_str()), msgpack(v)))
                .collect(),
        ),
    }
}

fn post(addr: &str, key: &str, body: &[u8]) -> std::io::Result<u16> {
    let mut s = TcpStream::connect(addr)?;
    write!(
        s,
        "POST / HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {key}\r\nContent-Type: application/msgpack\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    s.write_all(body)?;
    let mut response = String::new();
    s.read_to_string(&mut response)?;
    Ok(response
        .split(' ')
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let addr = args[0].clone();
    let key = args[1].clone();
    let total: usize = args[2].parse().unwrap();
    let batch: usize = args[3].parse().unwrap();
    let workers: usize = args[4].parse().unwrap();

    let started = Instant::now();
    let start_ms = chrono::Utc::now().timestamp_millis() - (total as i64 / 50);
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let (mut raw_ndjson, mut raw_msgpack) = (0usize, 0usize);
    let mut bodies = Vec::new();
    for b in 0..total.div_ceil(batch) {
        let mut rows = Vec::with_capacity(batch);
        for i in b * batch..((b + 1) * batch).min(total) {
            let r = row(&mut rng, i, start_ms);
            raw_ndjson += serde_json::to_vec(&r).unwrap().len() + 1;
            let r = msgpack(&r);
            let mut buf = Vec::new();
            rmpv::encode::write_value(&mut buf, &r).unwrap();
            raw_msgpack += buf.len();
            rows.push(r);
        }
        let mut packed = Vec::new();
        rmpv::encode::write_value(&mut packed, &rmpv::Value::Array(rows)).unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&packed).unwrap();
        bodies.push(gz.finish().unwrap());
    }
    let gzipped: usize = bodies.iter().map(Vec::len).sum();
    eprintln!(
        "generated {total} rows in {:.1}s: ndjson {raw_ndjson} B ({} B/row), msgpack {raw_msgpack} B, gzip {gzipped} B",
        started.elapsed().as_secs_f64(),
        raw_ndjson / total
    );

    let bodies = Arc::new(bodies);
    let next = Arc::new(AtomicUsize::new(0));
    let throttled = Arc::new(AtomicUsize::new(0));
    let failures = Arc::new(Mutex::new(Vec::new()));
    let sending = Instant::now();
    let threads: Vec<_> = (0..workers)
        .map(|_| {
            let (bodies, next, throttled, failures, addr, key) = (
                bodies.clone(),
                next.clone(),
                throttled.clone(),
                failures.clone(),
                addr.clone(),
                key.clone(),
            );
            std::thread::spawn(move || {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(body) = bodies.get(i) else { break };
                    loop {
                        match post(&addr, &key, body) {
                            Ok(202) => break,
                            Ok(429) => {
                                throttled.fetch_add(1, Ordering::Relaxed);
                                std::thread::sleep(Duration::from_millis(50));
                            }
                            other => {
                                failures.lock().unwrap().push(format!("{other:?}"));
                                break;
                            }
                        }
                    }
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    let secs = sending.elapsed().as_secs_f64();
    let failures = failures.lock().unwrap();
    println!(
        "accepted {total} rows in {secs:.2}s = {:.0} rows/s, {} responses 429, {} failures {:?}",
        total as f64 / secs,
        throttled.load(Ordering::Relaxed),
        failures.len(),
        failures.iter().take(3).collect::<Vec<_>>()
    );
    println!("raw_ndjson_bytes={raw_ndjson}");
}
