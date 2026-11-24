//! What ingest counts, once a queue took it.

use super::*;

/// Answers each queued job until `n` arrived.
async fn answer(s: &Server, n: usize) {
    let mut seen = 0;
    while seen < n {
        if let Ok(write) = s.error_inbox.try_recv() {
            drop(write.done.send(Ok(())));
            seen += 1;
        }
        tokio::task::yield_now().await;
    }
}

/// Project, signal, records and bytes of every count, emptying the counter.
fn counts(s: &Server) -> Vec<(i64, &'static str, u64, u64)> {
    s.usage
        .take()
        .into_iter()
        .map(|r| (r.project, r.signal.name(), r.count.records, r.count.bytes))
        .collect()
}

const ENVELOPE: &str = concat!(
    "{}\n",
    "{\"type\":\"event\"}\n",
    "{\"message\":\"boom\"}\n",
    "{\"type\":\"session\"}\n",
    "{\"sid\":\"6e3a9a0bd0a14e4f8a6e1c2d3b4a5f60\",\"init\":true,\"started\":\"2024-10-02T07:12:30Z\",\"status\":\"ok\",\"errors\":0,\"attrs\":{\"release\":\"web@3\"}}\n",
    "{\"type\":\"feedback\"}\n",
    "{\"type\":\"feedback\",\"contexts\":{\"feedback\":{\"message\":\"hi\"}}}\n",
    "{\"type\":\"check_in\"}\n",
    "{\"check_in_id\":\"42424242-4242-4242-4242-424242424242\",\"monitor_slug\":\"backup\",\"status\":\"ok\"}\n",
    "{\"type\":\"client_report\"}\n",
    "{\"discarded_events\":[]}\n",
    "{\"type\":\"log\"}\n",
    "{\"items\":[{\"timestamp\":1,\"level\":\"info\",\"body\":\"a\"},{\"timestamp\":2,\"level\":\"info\",\"body\":\"b\"}]}\n",
);

#[tokio::test]
async fn an_envelope_counts_each_signal_with_the_size_of_its_items() {
    let s = server();
    let gzipped = {
        use std::io::Write as _;
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(ENVELOPE.as_bytes()).unwrap();
        gz.finish().unwrap()
    };
    let (r, ()) = tokio::join!(
        s.send(post(
            "/api/1/envelope/?sentry_key=key-a",
            &[("content-encoding", "gzip")],
            gzipped
        )),
        answer(&s, 3)
    );
    assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
    // Sizes are the items after decompression. The client report is not
    // counted.
    assert_eq!(
        counts(&s),
        [
            (1, "errors", 1, 18),
            (1, "logs", 2, 95),
            (1, "sessions", 1, 140),
            (1, "feedback", 1, 60),
            (1, "checkins", 1, 92),
        ]
    );
}

#[tokio::test]
async fn log_routes_count_records_and_the_decompressed_body() {
    let s = server();
    let logtail = post(
        "/",
        &[
            ("authorization", "Bearer key-b"),
            ("content-type", "application/msgpack"),
            ("content-encoding", "gzip"),
        ],
        fixture("logtail-pino/1.body"),
    );
    assert_eq!(s.send(logtail).await.status, 202);
    // The queue holds 3 records.
    assert_eq!(s.logs.try_recv().unwrap().len(), 3);
    let body = "{\"resourceLogs\":[{\"scopeLogs\":[{\"logRecords\":[{\"body\":{\"stringValue\":\"hi\"}}]}]}]}";
    assert_eq!(s.send(otlp("application/json", body)).await.status, 200);
    assert_eq!(counts(&s), [(1, "logs", 1, 81), (2, "logs", 3, 534)]);
    let legacy = post(
        "/api/2/store/?sentry_key=key-b",
        &[],
        "{\"message\":\"boom\"}",
    );
    let (r, ()) = tokio::join!(s.send(legacy), answer(&s, 1));
    assert_eq!(r.status, 200);
    assert_eq!(counts(&s), [(2, "errors", 1, 18)]);
}

#[tokio::test]
async fn a_refused_request_counts_nothing() {
    let s = server();
    s.errors.try_send(dummy_write()).unwrap();
    let r = s
        .send(post(
            "/api/1/envelope/?sentry_key=key-a",
            &[],
            "{}\n{\"type\":\"event\"}\n{\"message\":\"boom\"}\n",
        ))
        .await;
    assert_eq!(r.status, 429);
    let r = s
        .send(post("/api/1/envelope/?sentry_key=wrong", &[], ENVELOPE))
        .await;
    assert_eq!(r.status, 403);
    assert_eq!(counts(&s), []);
}
