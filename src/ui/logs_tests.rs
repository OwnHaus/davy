//! The contract of the log search routes.

use super::{Reply, Ui, ui};
use crate::{
    logs::{LogLevel, LogRecord, LogSource, store::LogWriter},
    store, time,
};
use serde_json::{Map, Value, json};
use std::sync::Arc;

/// A time with every nanosecond digit set, past what a JavaScript number
/// holds exactly.
const AT: i64 = 1_763_600_000_123_456_789;

fn shop() -> (Ui, String) {
    let s = ui();
    store::create_project(&s.db, "shop").unwrap();
    let cookie = s.person("ann@example.com", time::now_ms());
    (s, cookie)
}

fn log(timestamp: i64, level: LogLevel, message: &str) -> LogRecord {
    LogRecord {
        project_id: 1,
        timestamp,
        observed_at: timestamp,
        source: LogSource::Otlp,
        level,
        message: message.into(),
        service: Some("checkout".into()),
        trace_id: None,
        span_id: None,
        attributes: Map::new(),
    }
}

/// Writes `batch` to the log store of `s` and waits until it is searchable.
fn write(s: &Ui, batch: Vec<LogRecord>) {
    let dir = crate::logs::dir(s.reads.data());
    let writer = LogWriter::start(
        dir,
        crate::logs::store::MAX_RETENTION_DAYS,
        crate::logs::store::no_overrides(),
    )
    .unwrap();
    assert!(writer.queue.push(batch).is_ok());
    drop(writer.queue);
    writer.thread.join().unwrap();
}

async fn read(s: &Ui, cookie: &str, uri: &str) -> Reply {
    s.get(uri, &[("cookie", cookie)]).await
}

fn messages(body: &Value) -> Vec<&str> {
    let logs = body["logs"].as_array().unwrap();
    logs.iter()
        .map(|l| l["message"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn a_log_time_keeps_every_nanosecond_through_the_api() {
    let (s, cookie) = shop();
    write(&s, vec![log(AT, LogLevel::Warn, "card declined")]);
    let window = "from=1763600000000000000&to=1763600000999999999";
    let r = read(&s, &cookie, &format!("/ui/api/logs?{window}")).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        r.body,
        json!({
            "logs": [{
                "timestamp": "2025-11-20T00:53:20.123456789Z",
                "timestamp_ns": "1763600000123456789",
                "project": "shop",
                "source": "otlp",
                "level": "warn",
                "service": "checkout",
                "message": "card declined",
                "trace_id": null,
                "span_id": null,
                "attributes": {},
            }],
            "before": null,
        })
    );
    let r = read(&s, &cookie, &format!("/ui/api/logs/histogram?{window}")).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let mut counts = vec![0; 200];
    counts[24] = 1;
    assert_eq!(
        r.body,
        json!({ "from": "1763600000000000000", "step": "5000000", "counts": counts }),
        "5 ms buckets, the log 123 ms in"
    );
}

#[tokio::test]
async fn pages_go_back_without_repeating_logs_that_share_a_nanosecond() {
    let (s, cookie) = shop();
    let mut batch: Vec<_> = (0..120)
        .map(|i| log(AT, LogLevel::Info, &format!("same {i}")))
        .collect();
    batch.extend((0..100).map(|i| log(AT - 1000 * (i + 1), LogLevel::Info, &format!("old {i}"))));
    write(&s, batch);
    let base = format!("/ui/api/logs?from={}&to={AT}", AT - 1_000_000);
    let mut seen: Vec<String> = Vec::new();
    let mut uri = base.clone();
    let mut sizes = Vec::new();
    loop {
        let r = read(&s, &cookie, &uri).await;
        assert_eq!(r.status, 200, "{}", r.body["error"]);
        sizes.push(messages(&r.body).len());
        seen.extend(messages(&r.body).into_iter().map(str::to_owned));
        match r.body["before"].as_str() {
            Some(before) => uri = format!("{base}&before={before}"),
            None => break,
        }
    }
    assert_eq!(sizes, [100, 100, 20]);
    let unique: std::collections::HashSet<_> = seen.iter().collect();
    assert_eq!(unique.len(), 220, "every log once");
    assert_eq!(
        seen.last().map(String::as_str),
        Some("old 99"),
        "oldest last"
    );

    let newer = |after: i64| format!("{base}&after={after}");
    let r = read(&s, &cookie, &newer(AT)).await;
    assert_eq!(
        messages(&r.body),
        Vec::<&str>::new(),
        "nothing after the newest"
    );
    let r = read(&s, &cookie, &newer(AT - 1001)).await;
    assert_eq!(messages(&r.body).len(), 100, "a full page of newer logs");
    assert!(r.body["before"].is_string(), "and the way back from it");
    let uri = format!("/ui/api/logs?to={}&after={}", AT - 1, AT - 2001);
    let r = read(&s, &cookie, &uri).await;
    assert_eq!(messages(&r.body), ["old 0", "old 1"], "newest first");
}

#[tokio::test]
async fn pages_go_on_past_more_logs_at_one_nanosecond_than_a_cursor_counts() {
    let (s, cookie) = shop();
    let mut batch: Vec<_> = (0..10_050)
        .map(|i| log(AT, LogLevel::Info, &format!("same {i}")))
        .collect();
    batch.extend((0..5).map(|i| log(AT - 1000 * (i + 1), LogLevel::Info, &format!("old {i}"))));
    write(&s, batch);
    let base = format!("/ui/api/logs?from={}&to={AT}", AT - 1_000_000);
    let (mut uri, mut pages) = (base.clone(), 0);
    let last = loop {
        let r = read(&s, &cookie, &uri).await;
        assert_eq!(r.status, 200, "{}", r.body["error"]);
        pages += 1;
        match r.body["before"].as_str() {
            Some(before) => uri = format!("{base}&before={before}"),
            None => {
                break messages(&r.body)
                    .into_iter()
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
            }
        }
    };
    assert_eq!(
        pages, 101,
        "100 pages at one nanosecond, then the older logs"
    );
    assert_eq!(last, ["old 0", "old 1", "old 2", "old 3", "old 4"]);

    let uri = format!("/ui/api/logs?to={}&before={AT}.50", AT - 1);
    let r = read(&s, &cookie, &uri).await;
    assert_eq!(
        messages(&r.body).first(),
        Some(&"old 0"),
        "a cursor past the window skips nothing in it"
    );
}

#[tokio::test]
async fn the_filter_narrows_both_the_page_and_the_histogram() {
    let (s, cookie) = shop();
    store::create_project(&s.db, "admin").unwrap();
    let mut other = log(AT - 2, LogLevel::Error, "admin export failed");
    other.project_id = 2;
    write(
        &s,
        vec![
            log(AT - 3, LogLevel::Info, "cart opened"),
            log(AT - 1, LogLevel::Error, "payment timed out"),
            other,
        ],
    );
    let window = format!("from={}&to={AT}", AT - 10);
    let cases = [
        (
            "",
            vec!["payment timed out", "admin export failed", "cart opened"],
        ),
        ("&project=shop", vec!["payment timed out", "cart opened"]),
        (
            "&level=error",
            vec!["payment timed out", "admin export failed"],
        ),
        ("&q=timed", vec!["payment timed out"]),
        (
            "&q=export+OR+cart&project=admin",
            vec!["admin export failed"],
        ),
    ];
    for (filter, expected) in cases {
        let r = read(&s, &cookie, &format!("/ui/api/logs?{window}{filter}")).await;
        assert_eq!(messages(&r.body), expected, "{filter}");
        let uri = format!("/ui/api/logs/histogram?{window}{filter}");
        let r = read(&s, &cookie, &uri).await;
        let total: u64 = r.body["counts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n.as_u64().unwrap())
            .sum();
        assert_eq!(total, expected.len() as u64, "histogram {filter}");
    }
}

#[tokio::test]
async fn the_log_routes_refuse_what_they_do_not_take() {
    let (s, cookie) = shop();
    let cases = [
        (
            "/ui/api/logs?sort=old",
            400,
            "this route takes only q, from, to, project, level, before, after",
        ),
        (
            "/ui/api/logs/histogram?before=1.1",
            400,
            "this route takes only q, from, to, project, level",
        ),
        ("/ui/api/logs?q=a&q=b", 400, "q is given twice"),
        ("/ui/api/logs?from=2&to=1", 400, "from is after to"),
        (
            "/ui/api/logs?to=-1",
            400,
            "to is a time in nanoseconds since 1970",
        ),
        (
            "/ui/api/logs?to=1.5",
            400,
            "to is a time in nanoseconds since 1970",
        ),
        ("/ui/api/logs?level=loud", 400, "unknown level \"loud\""),
        (
            "/ui/api/logs?before=nope",
            400,
            "before is the cursor of a page of logs",
        ),
        (
            "/ui/api/logs?before=5.1&after=4",
            400,
            "a page is before or after, not both",
        ),
        ("/ui/api/logs?project=nowhere", 404, "no project nowhere"),
        (
            "/ui/api/logs/histogram?project=nowhere",
            404,
            "no project nowhere",
        ),
    ];
    for (uri, status, error) in cases {
        let r = read(&s, &cookie, uri).await;
        assert_eq!(
            (r.status, &r.body),
            (status, &json!({ "error": error })),
            "{uri}"
        );
    }
    let r = read(&s, &cookie, "/ui/api/logs?q=attributes.a%3A%5B1").await;
    assert_eq!(r.status, 400, "a query that does not parse: {}", r.body);
    let long = "a".repeat(1001);
    let r = read(&s, &cookie, &format!("/ui/api/logs?q={long}")).await;
    assert_eq!(
        r.body,
        json!({ "error": "the query has at most 1000 characters" })
    );
}

#[tokio::test]
async fn a_ninth_log_search_or_histogram_at_once_waits() {
    let (s, cookie) = shop();
    let busy = Arc::clone(&s.reads.searches)
        .try_acquire_many_owned(8)
        .unwrap();
    for uri in ["/ui/api/logs", "/ui/api/logs/histogram"] {
        let r = read(&s, &cookie, uri).await;
        assert_eq!((r.status, r.header("retry-after")), (429, "1"), "{uri}");
    }
    // A search turned away is not recorded: the writer applies a batch
    // whole before it answers, so the search would be stored by now.
    let barrier = store::Job::First {
        what: crate::firsts::First::Log,
        at: 1,
    };
    store::submit(&s.writer.ui, barrier).await.unwrap();
    let recorded: i64 =
        s.db.query_row("SELECT count(*) FROM searches", [], |r| r.get(0))
            .unwrap();
    assert_eq!(recorded, 0);
    drop(busy);
    for uri in ["/ui/api/logs", "/ui/api/logs/histogram"] {
        assert_eq!(read(&s, &cookie, uri).await.status, 200, "{uri}");
    }
}

/// `q` for a query string, every byte but letters and digits escaped.
fn escaped(q: &str) -> String {
    q.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

#[tokio::test]
async fn the_attribute_searches_the_logs_page_builds_find_their_log() {
    let (s, cookie) = shop();
    let mut hit = log(AT, LogLevel::Info, "order placed");
    hit.attributes = serde_json::from_value(json!({
        "order.id": "ord-1001",
        "http": {"status": 502},
        "ok": true,
        "provider": "acme-pay",
    }))
    .unwrap();
    write(&s, vec![hit, log(AT - 1, LogLevel::Info, "other")]);
    let window = format!("from={}&to={AT}", AT - 1_000_000);
    for (q, expected) in [
        (r#"attributes.order\.id:"ord-1001""#, &["order placed"][..]),
        ("attributes.http.status:502", &["order placed"]),
        ("attributes.ok:true", &["order placed"]),
        (r#"attributes.provider:"acme-pay""#, &["order placed"]),
        (r#"attributes.provider:"acme-other""#, &[]),
    ] {
        let uri = format!("/ui/api/logs?{window}&q={}", escaped(q));
        let r = read(&s, &cookie, &uri).await;
        assert_eq!(r.status, 200, "{q}: {}", r.body["error"]);
        assert_eq!(messages(&r.body), expected, "{q}");
    }
}

#[tokio::test]
async fn the_first_page_is_recorded_as_a_search_and_later_pages_are_not() {
    let (s, cookie) = shop();
    let window = format!("from={}&to={AT}", AT - 1_000_000);
    for uri in [
        format!("/ui/api/logs?{window}&q=card&project=shop&level=warn"),
        format!("/ui/api/logs?{window}&after={}", AT - 10),
        format!("/ui/api/logs?{window}&before={AT}.0"),
        format!("/ui/api/logs/histogram?{window}"),
    ] {
        let r = read(&s, &cookie, &uri).await;
        assert_eq!(r.status, 200, "{uri}: {}", r.body);
    }
    // The writer applies jobs in order, so once this one is done the
    // search queued before it is stored.
    store::submit(
        &s.writer.ui,
        store::Job::First {
            what: crate::firsts::First::Log,
            at: 1,
        },
    )
    .await
    .unwrap();
    let rows: Vec<(String, i64, i64, i64, i64, String)> =
        s.db.prepare("SELECT by, project_id, since, until, level, query FROM searches")
            .unwrap()
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
    assert_eq!(
        rows,
        [("ui".into(), 1, AT - 1_000_000, AT, 4, "card".into())]
    );
}
