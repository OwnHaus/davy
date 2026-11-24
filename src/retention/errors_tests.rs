use super::*;
use crate::{
    event::{ErrorEvent, EventId},
    items::{SdkItem, user_feedback::UserFeedback},
    retention::{By, Change, DAY_MS, Defaults, ServerDefault, Setting},
    store::{self, Job, Write, Writer},
};
use serde_json::json;

/// A time in 2027, so cutoffs a few days back are after 1970.
const BASE: i64 = 1_800_000_000_000;

fn day(n: i64) -> i64 {
    BASE + n * DAY_MS
}

/// A server's writer on a data directory of its own, and a connection to
/// read what it wrote.
struct Server {
    root: std::path::PathBuf,
    writer: Writer,
    c: Connection,
}

impl Server {
    fn new(name: &str, projects: &[&str]) -> Self {
        let root = crate::test_support::scratch(name);
        let writer = Writer::start(store::open(&root).unwrap(), 8);
        let c = store::open(&root).unwrap();
        for p in projects {
            store::create_project(&c, p).unwrap();
        }
        Self { root, writer, c }
    }

    fn run(&self, job: Job) -> Result<(), String> {
        let (done, outcome) = tokio::sync::oneshot::channel();
        self.writer.sender.send(Write { job, done }).unwrap();
        outcome.blocking_recv().unwrap().map(drop)
    }

    fn ingest(&self, project: i64, events: Vec<ErrorEvent>) {
        let held = crate::http::body::Budget::new(0).hold();
        self.run(Job::Ingest {
            project,
            events,
            held,
        })
        .unwrap();
    }

    fn sweep(&self, at: i64, error_days: u32) {
        let error_days = Days::new(error_days).unwrap();
        self.run(Job::Sweep { at, error_days }).unwrap();
    }

    fn number(&self, sql: &str) -> i64 {
        self.c.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    fn rows(&self, sql: &str) -> Vec<String> {
        let mut q = self.c.prepare(sql).unwrap();
        q.query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        drop(std::fs::remove_dir_all(&self.root));
    }
}

/// An error with `message`, received and thrown at `at`.
fn error(message: &str, at: i64, environment: &str, release: &str) -> ErrorEvent {
    let v = json!({"message": message, "environment": environment, "release": release});
    let mut e = ErrorEvent::parse(v).unwrap();
    e.received_at = at;
    e.timestamp = at;
    e
}

#[test]
fn an_expired_event_leaves_the_payload_sample_alone() {
    let s = Server::new("errors-sample", &["api"]);
    // Events 0 to 99 of one issue, a millisecond apart. The one at 99 is in
    // the sample and keeps its payload for good.
    let at = |n| day(1) + n;
    s.ingest(
        1,
        (0..100)
            .map(|n| error("timeout", at(n), "prod", "v1"))
            .collect(),
    );
    let kept = |n: i64| {
        let sql = format!(
            "SELECT payload IS NOT NULL FROM events WHERE received_at={}",
            at(n)
        );
        s.number(&sql) == 1
    };
    assert_eq!(
        (kept(49), kept(50), kept(98), kept(99)),
        (true, false, false, true)
    );
    // Event 0 expires, so the issue counts 99 events, as many as before
    // event 99 arrived.
    s.sweep(at(1) + 10 * DAY_MS, 10);
    assert_eq!(s.number("SELECT event_count FROM issues"), 99);
    s.ingest(1, vec![error("timeout", at(100), "prod", "v1")]);
    assert!(kept(99), "a payload in the sample was cleared");
    // Event 101 is outside the sample, so it keeps its payload, marked,
    // only while it is the newest. The decoders skip the mark.
    s.ingest(1, vec![error("timeout", at(101), "prod", "v1")]);
    let issue = s.number("SELECT id FROM issues");
    let raw = store::payload(&s.c, issue).unwrap().unwrap();
    assert_eq!(raw["message"], "timeout");
    assert!(kept(101));
    s.ingest(1, vec![error("timeout", at(102), "prod", "v1")]);
    let left = (kept(99), kept(100), kept(101), kept(102));
    assert_eq!(left, (true, true, false, true));
}

#[test]
fn expired_events_leave_their_issues_counts_first_seen_and_tags_true() {
    let s = Server::new("errors-counts", &["api"]);
    s.ingest(
        1,
        vec![
            error("timeout", day(1), "prod", "v1"),
            error("timeout", day(5), "prod", "v2"),
            error("timeout", day(20), "dev", "v2"),
            error("refused", day(2), "prod", "v1"),
        ],
    );
    s.sweep(day(16), 10);
    assert_eq!(s.rows("SELECT title FROM issues"), ["timeout"]);
    let (count, first, last): (i64, i64, i64) =
        s.c.query_row(
            "SELECT event_count, first_seen, last_seen FROM issues",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!((count, first, last), (1, day(20), day(20)));
    assert_eq!(s.number("SELECT count(*) FROM events"), 1);
    assert_eq!(s.number("SELECT sum(count) FROM issue_hourly"), 1);
    assert_eq!(s.number("SELECT count(*) FROM issue_hourly"), 1);
    assert_eq!(
        s.rows("SELECT key || '=' || value FROM issue_tags ORDER BY key"),
        ["environment=dev", "release=v2"]
    );
    // The issues list filters by what is left.
    let filter = |environment: &str| crate::issues::Filter {
        environment: Some(environment.into()),
        ..crate::issues::Filter::default()
    };
    let listed = |environment| {
        let sort = crate::issues::Sort::LastSeen;
        let page = crate::issues::list(&s.c, &filter(environment), sort, None, day(16));
        page.unwrap().issues.len()
    };
    assert_eq!((listed("prod"), listed("dev")), (0, 1));
}

#[test]
fn an_issue_whose_events_all_expired_goes_and_a_returning_error_is_new() {
    let s = Server::new("errors-forget", &["api"]);
    let thrown = error("timeout", day(1), "prod", "v1");
    let event_id = thrown.event_id.0.clone();
    s.ingest(1, vec![thrown]);
    let feedback = UserFeedback {
        event_id: Some(EventId(event_id)),
        message: "it hung".into(),
        name: None,
        email: None,
        url: None,
    };
    let items = vec![SdkItem::UserFeedback(feedback)];
    s.run(Job::Items {
        project: 1,
        at: day(9),
        items,
    })
    .unwrap();
    let status = store::IssueStatus::Resolved {
        in_release: None,
        at: day(2),
    };
    let one = crate::issues::IssueIds::one(1);
    s.run(Job::SetStatus {
        issues: one,
        status,
    })
    .unwrap();
    s.sweep(day(12), 10);
    assert_eq!(s.number("SELECT count(*) FROM issues"), 0);
    assert_eq!(s.number("SELECT count(*) FROM issue_tags"), 0);
    assert_eq!(s.number("SELECT count(*) FROM issue_hourly"), 0);
    assert_eq!(
        s.number("SELECT count(*) FROM user_feedback WHERE issue_id IS NULL"),
        1
    );
    // The release is still known, so a fix in it still counts.
    assert_eq!(s.number("SELECT count(*) FROM releases"), 1);
    s.ingest(1, vec![error("timeout", day(12), "prod", "v1")]);
    let (id, status): (i64, String) =
        s.c.query_row("SELECT id, status FROM issues", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_ne!(id, 1);
    assert_eq!(status, "unresolved");
    s.sweep(day(20), 10);
    assert_eq!(s.number("SELECT count(*) FROM user_feedback"), 0);
    assert_eq!(s.number("SELECT count(*) FROM issues"), 1);
}

#[test]
fn the_server_default_keeps_errors_until_a_project_says_otherwise() {
    let s = Server::new("errors-setting", &["api", "web"]);
    for project in [1, 2] {
        s.ingest(project, vec![error("timeout", day(1), "prod", "v1")]);
    }
    let default = Defaults::default().errors.get();
    s.sweep(day(400), default);
    assert_eq!(s.number("SELECT count(*) FROM events"), 2);
    let change = |errors| Change {
        project: "api".into(),
        logs_days: None,
        errors_days: Some(errors),
    };
    s.run(Job::SetRetention(change(Setting::Days(30)), By::Person))
        .unwrap();
    s.sweep(day(400), default);
    assert_eq!(
        s.rows("SELECT slug FROM projects JOIN events ON project_id=projects.id"),
        ["web"]
    );
    let reset = Setting::Default(ServerDefault::Default);
    s.run(Job::SetRetention(change(reset), By::Person)).unwrap();
    assert_eq!(
        s.number("SELECT count(*) FROM projects WHERE error_retention_days IS NULL"),
        2
    );
    let unknown = Job::SetRetention(
        Change {
            project: "nope".into(),
            ..change(Setting::Days(3))
        },
        By::Person,
    );
    assert_eq!(s.run(unknown), Err(crate::retention::unknown("nope")));
    // A refused change fails only itself: the writer goes on.
    s.ingest(2, vec![error("refused", day(400), "prod", "v1")]);
    assert_eq!(s.number("SELECT count(*) FROM events"), 2);
}

#[test]
fn a_sweep_deletes_one_batch_per_class_then_goes_on_within_its_budget() {
    let s = Server::new("errors-budget", &["api"]);
    for chunk in 0..3 {
        let events = (0..400)
            .map(|n| error("timeout", day(1) + chunk * 400 + n, "prod", "v1"))
            .collect();
        s.ingest(1, events);
    }
    let days = Days::new(10).unwrap();
    sweep(&s.c, day(20), days, Duration::ZERO).unwrap();
    assert_eq!(s.number("SELECT count(*) FROM events"), 1200 - BATCH);
    assert_eq!(s.number("SELECT event_count FROM issues"), 1200 - BATCH);
    sweep(&s.c, day(20), days, Duration::from_secs(60)).unwrap();
    assert_eq!(s.number("SELECT count(*) FROM events"), 0);
    assert_eq!(s.number("SELECT count(*) FROM issues"), 0);
}

#[test]
fn a_tag_stays_while_an_event_that_arrived_with_the_last_deleted_carries_it() {
    let s = Server::new("errors-tied-tag", &["api"]);
    // One request's events arrive in the same millisecond, more of them
    // than one batch deletes.
    s.ingest(
        1,
        (0..250)
            .map(|_| error("timeout", day(1), "prod", "v1"))
            .collect(),
    );
    let days = Days::new(10).unwrap();
    sweep(&s.c, day(20), days, Duration::ZERO).unwrap();
    assert_eq!(s.number("SELECT count(*) FROM events"), 250 - BATCH);
    assert_eq!(
        s.rows("SELECT key || '=' || value FROM issue_tags ORDER BY key"),
        ["environment=prod", "release=v1"]
    );
    sweep(&s.c, day(20), days, Duration::from_secs(60)).unwrap();
    assert_eq!(s.number("SELECT count(*) FROM issue_tags"), 0);
}

#[test]
fn first_and_last_seen_follow_the_events_left_whatever_their_clocks_said() {
    let s = Server::new("errors-seen", &["api"]);
    // Received on day 1 from a clock 8 days ahead, on day 7, and on day 8
    // from a clock 5 days behind.
    for (received, thrown) in [(1, 9), (7, 7), (8, 3)] {
        let mut e = error("timeout", day(received), "prod", "v1");
        e.timestamp = day(thrown);
        s.ingest(1, vec![e]);
    }
    s.sweep(day(16), 10);
    let seen: (i64, i64) =
        s.c.query_row("SELECT first_seen, last_seen FROM issues", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(seen, (day(3), day(7)));
    // Each is one lookup in an index, not a read of the issue's events.
    let plan = plan(&s.c, SEEN, [1]);
    let indexed = plan.iter().filter(|d| d.contains("events_issue_timestamp"));
    assert_eq!(indexed.count(), 2, "{plan:?}");
}

#[test]
fn a_batch_stops_at_its_work_however_its_events_spread_over_issues() {
    let s = Server::new("errors-spread", &["api"]);
    // Digits in a message are grouped as one, so the issues take letters.
    let letter = |n: i64| char::from(b'a' + u8::try_from(n).unwrap());
    let events = (0..200)
        .map(|n| {
            let message = format!("timeout {}{}", letter(n / 26), letter(n % 26));
            error(&message, day(1) + n, "prod", "v1")
        })
        .collect();
    s.ingest(1, events);
    assert_eq!(s.number("SELECT count(*) FROM issues"), 200);
    let days = Days::new(10).unwrap();
    sweep(&s.c, day(20), days, Duration::ZERO).unwrap();
    // Each event is the first of its issue in the batch.
    let left = 200 - WORK / (ISSUE_WORK + 1);
    assert_eq!(s.number("SELECT count(*) FROM events"), left);
    assert_eq!(s.number("SELECT count(*) FROM issues"), left);
    sweep(&s.c, day(20), days, Duration::from_secs(60)).unwrap();
    assert_eq!(s.number("SELECT count(*) FROM issues"), 0);
}

#[test]
fn a_row_that_still_points_at_an_issue_keeps_it_with_no_events() {
    let s = Server::new("errors-pinned", &["api"]);
    s.ingest(1, vec![error("timeout", day(1), "prod", "v1")]);
    s.c.execute_batch(
        "CREATE TABLE pins(issue_id INTEGER NOT NULL REFERENCES issues(id)); INSERT INTO pins VALUES (1);",
    )
    .unwrap();
    s.sweep(day(20), 10);
    assert_eq!(s.number("SELECT count(*) FROM events"), 0);
    assert_eq!(s.number("SELECT event_count FROM issues WHERE id=1"), 0);
    // The writer's batch went through, so ingest goes on.
    s.ingest(1, vec![error("timeout", day(20), "prod", "v1")]);
    assert_eq!(s.number("SELECT event_count FROM issues WHERE id=1"), 1);
}

/// Links `n` user feedback, received at `at`, to the issue of `event`.
fn feedback_on(s: &Server, event: &str, n: usize, at: i64) {
    let items = (0..n)
        .map(|_| {
            SdkItem::UserFeedback(UserFeedback {
                event_id: Some(EventId(event.into())),
                message: "it hung".into(),
                name: None,
                email: None,
                url: None,
            })
        })
        .collect();
    s.run(Job::Items {
        project: 1,
        at,
        items,
    })
    .unwrap();
}

#[test]
fn an_issue_with_much_feedback_is_unlinked_a_batch_at_a_time_then_goes() {
    let s = Server::new("errors-unlink", &["api"]);
    let thrown = error("timeout", day(1), "prod", "v1");
    let event_id = thrown.event_id.0.clone();
    s.ingest(1, vec![thrown]);
    feedback_on(&s, &event_id, 120, day(15));
    let linked = || s.number("SELECT count(*) FROM user_feedback WHERE issue_id IS NOT NULL");
    let issue = || {
        s.c.query_row("SELECT count(*), sum(event_count) FROM issues", [], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?))
        })
        .unwrap()
    };
    let days = Days::new(10).unwrap();
    // Each sweep with no budget does one batch of each class, so one batch
    // unlinks at most UNLINK, and the issue stays with no events till the
    // last goes.
    sweep(&s.c, day(20), days, Duration::ZERO).unwrap();
    assert_eq!((linked(), issue()), (70, (1, Some(0))));
    sweep(&s.c, day(20), days, Duration::ZERO).unwrap();
    assert_eq!((linked(), issue()), (20, (1, Some(0))));
    sweep(&s.c, day(20), days, Duration::ZERO).unwrap();
    assert_eq!((linked(), issue()), (0, (0, None)));
    assert_eq!(s.number("SELECT count(*) FROM user_feedback"), 120);
    assert_eq!(s.number("SELECT id FROM id_floor WHERE name='issues'"), 1);
}

/// The types of the notices queued so far, oldest first.
fn notices(s: &Server) -> Vec<String> {
    s.rows(
        "SELECT type || ' ' || json_extract(payload, '$.issue.id') FROM notifications ORDER BY id",
    )
}

/// A webhook channel for every project, so issue notices are queued.
fn channel(s: &Server) {
    let hook = crate::monitor::CheckUrl::parse("https://hooks.example/x").unwrap();
    crate::alert::store::add(
        &s.c,
        None,
        &crate::alert::Destination::Webhook(hook),
        false,
        0,
    )
    .unwrap();
}

#[test]
fn an_issue_whose_feedback_expires_with_its_last_event_goes_and_returns_new() {
    let s = Server::new("errors-unlink-same-sweep", &["api"]);
    channel(&s);
    let thrown = error("timeout", day(1), "prod", "v1");
    let event_id = thrown.event_id.0.clone();
    s.ingest(1, vec![thrown]);
    s.c.execute("UPDATE issues SET status='ignored'", [])
        .unwrap();
    // The SDK sends the feedback a second after the event, so one sweep
    // finds both expired.
    feedback_on(&s, &event_id, 1, day(1) + 1_000);
    sweep(
        &s.c,
        day(20),
        Days::new(10).unwrap(),
        Duration::from_secs(60),
    )
    .unwrap();
    assert_eq!(s.number("SELECT count(*) FROM issues"), 0);
    assert_eq!(s.number("SELECT id FROM id_floor WHERE name='issues'"), 1);
    s.ingest(1, vec![error("timeout", day(20), "prod", "v1")]);
    assert_eq!(
        s.rows("SELECT id || ' ' || status FROM issues"),
        ["2 unresolved"]
    );
    assert_eq!(notices(&s), ["issue.new 1", "issue.new 2"]);
}

#[test]
fn an_issue_with_no_events_but_new_ones_keeps_its_feedback() {
    let s = Server::new("errors-unlink-back", &["api"]);
    let thrown = error("timeout", day(1), "prod", "v1");
    let event_id = thrown.event_id.0.clone();
    s.ingest(1, vec![thrown]);
    feedback_on(&s, &event_id, 60, day(15));
    let days = Days::new(10).unwrap();
    sweep(&s.c, day(20), days, Duration::ZERO).unwrap();
    // The error comes back before the last feedback is unlinked, so the
    // issue is no longer empty and keeps what is still linked.
    s.ingest(1, vec![error("timeout", day(20), "prod", "v1")]);
    sweep(&s.c, day(20), days, Duration::from_secs(60)).unwrap();
    let left = s.number("SELECT count(*) FROM user_feedback WHERE issue_id=1");
    assert_eq!(
        (left, s.number("SELECT event_count FROM issues WHERE id=1")),
        (10, 1)
    );
}

#[test]
fn an_error_back_before_its_feedback_is_unlinked_starts_its_issue_again() {
    let s = Server::new("errors-unlink-revive", &["api"]);
    channel(&s);
    let thrown = error("timeout", day(1), "prod", "v1");
    let event_id = thrown.event_id.0.clone();
    s.ingest(1, vec![thrown]);
    s.c.execute("UPDATE issues SET status='ignored'", [])
        .unwrap();
    feedback_on(&s, &event_id, 60, day(15));
    sweep(&s.c, day(20), Days::new(10).unwrap(), Duration::ZERO).unwrap();
    let mut back = error("timeout", day(20), "prod", "v2");
    back.timestamp = day(19);
    s.ingest(1, vec![back]);
    // It keeps its id and the feedback still linked, and is otherwise as
    // new as an issue deleted already would be.
    let issue: (i64, String, i64, i64, i64) =
        s.c.query_row(
            "SELECT id, status, first_seen, last_seen, event_count FROM issues",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    assert_eq!(issue, (1, "unresolved".into(), day(19), day(19), 1));
    assert_eq!(notices(&s), ["issue.new 1", "issue.new 1"]);
}

#[test]
fn finding_the_feedback_of_issues_with_no_events_reads_only_those() {
    let s = Server::new("errors-unlink-plan", &["api"]);
    let plan = plan(&s.c, LINKED, [1]);
    let uses = |index: &str| plan.iter().any(|d| d.contains(index));
    assert_eq!(
        (uses("issues_emptied"), uses("user_feedback_issue")),
        (true, true),
        "{plan:?}"
    );
}

#[test]
fn a_release_or_environment_over_its_limit_is_dropped_and_the_event_kept() {
    let s = Server::new("errors-caps", &["api"]);
    let (release, environment) = ("r".repeat(200), "e".repeat(64));
    s.ingest(1, vec![error("timeout", day(1), &environment, &release)]);
    let (long_release, long_environment) = ("r".repeat(201), "e".repeat(65));
    let over = error("timeout", day(2), &long_environment, &long_release);
    assert_eq!(
        (over.release.as_deref(), over.environment.as_deref()),
        (None, None)
    );
    s.ingest(1, vec![over]);
    assert_eq!(s.number("SELECT count(*) FROM events"), 2);
    assert_eq!(
        s.rows("SELECT key || '=' || length(value) FROM issue_tags ORDER BY key"),
        ["environment=64", "release=200"]
    );
    assert_eq!(s.number("SELECT length(version) FROM releases"), 200);
}

#[test]
fn the_caps_of_an_event_count_characters_not_bytes() {
    let parsed = |n: usize| {
        let v = "é".repeat(n);
        ErrorEvent::parse(json!({"release": v, "environment": v, "user": {"id": v}})).unwrap()
    };
    let kept = |n| {
        let e = parsed(n);
        let len = |v: Option<String>| v.map(|s| s.chars().count());
        (len(e.release), len(e.environment), len(e.user_id))
    };
    assert_eq!(kept(64), (Some(64), Some(64), Some(64)));
    assert_eq!(kept(65), (Some(65), None, Some(65)));
    assert_eq!(kept(200), (Some(200), None, Some(200)));
    assert_eq!(kept(201), (None, None, Some(201)));
    assert_eq!(kept(256), (None, None, Some(256)));
    assert_eq!(kept(257), (None, None, None));
}

#[test]
fn a_user_id_over_its_limit_is_dropped_and_the_event_kept() {
    let s = Server::new("errors-user-cap", &["api"]);
    let with_user = |id: String, at| {
        let mut e = ErrorEvent::parse(json!({"message": "timeout", "user": {"id": id}})).unwrap();
        e.received_at = at;
        e.timestamp = at;
        e
    };
    let (kept, over) = ("u".repeat(256), "u".repeat(1 << 20));
    s.ingest(1, vec![with_user(kept, day(1)), with_user(over, day(2))]);
    assert_eq!(
        s.rows("SELECT coalesce(CAST(length(user_id) AS TEXT), 'none') FROM events ORDER BY id"),
        ["256", "none"]
    );
}

#[test]
fn a_batch_prices_the_user_id_of_an_event_stored_before_its_cap() {
    let s = Server::new("errors-user-work", &["api"]);
    let events = (0..200)
        .map(|n| error("timeout", day(1) + n, "prod", "v1"))
        .collect();
    s.ingest(1, events);
    s.c.execute("UPDATE events SET user_id=hex(zeroblob(32768))", [])
        .unwrap();
    sweep(&s.c, day(20), Days::new(10).unwrap(), Duration::ZERO).unwrap();
    // 64 KiB of user id costs 16 units more, so the first event costs
    // 1 + 16 + ISSUE_WORK and each one after it 17, and 91 fit in WORK.
    assert_eq!(s.number("SELECT count(*) FROM events"), 109);
}

/// What `EXPLAIN QUERY PLAN` says for `sql`, one line per step.
fn plan(c: &Connection, sql: &str, params: impl rusqlite::Params) -> Vec<String> {
    let mut q = c.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
    q.query_map(params, |r| r.get(3))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

#[test]
fn picking_expired_events_and_untagging_read_the_index_by_project_and_time() {
    let s = Server::new("errors-pick-plan", &["api"]);
    let pick = plan(&s.c, PICK, params![day(20), 10, DAY_MS, BATCH]);
    let untag = plan(&s.c, UNTAG, params!["release", "v1", 1, day(1), 1]);
    let has = |plan: &[String], step: &str| plan.iter().any(|d| d.contains(step));
    // No sort of the whole range, and no scan of events or issue_tags.
    assert_eq!(
        (
            has(&pick, "events_project_received"),
            has(&pick, "TEMP B-TREE"),
            has(&untag, "events_project_received"),
            has(&untag, "SCAN"),
        ),
        (true, false, true, false),
        "{pick:?} {untag:?}"
    );
}
