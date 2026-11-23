use super::digest::{Digest, Entry};
use super::tests::{channel, incident, issue, memory, public_url};
use super::{Issue, Kind, Mail, Notice, store};

fn entry(regressed: bool, project: &str, id: i64, title: &str) -> Entry {
    Entry {
        regressed,
        project: project.into(),
        issue: Issue { id, ..issue(title) },
    }
}

fn two_projects() -> Digest {
    let bare = Issue {
        id: 5,
        culprit: None,
        release: None,
        ..issue("RangeError: y")
    };
    Digest {
        id: 7,
        at: 1_795_164_121_000,
        entries: vec![
            entry(false, "shop", 3, "TypeError: x"),
            Entry {
                regressed: true,
                project: "blog".into(),
                issue: bare,
            },
        ],
    }
}

#[test]
fn a_webhook_digest_lists_every_issue_with_its_project_type_and_link() {
    assert_eq!(
        two_projects().render(Kind::Webhook, &public_url()).unwrap(),
        r#"{"id":7,"type":"issue.digest","project":null,"at":"2026-11-20T08:42:01Z","issues":[{"type":"issue.new","project":"shop","url":"https://davy.example/ui/issues/3","issue":{"id":3,"title":"TypeError: x","culprit":"parseOrder(src/app.js)","level":"error","release":"shop@2.0.0"}},{"type":"issue.regressed","project":"blog","url":"https://davy.example/ui/issues/5","issue":{"id":5,"title":"RangeError: y","culprit":null,"level":"error","release":null}}]}"#
    );
}

#[test]
fn a_slack_digest_counts_the_issues_and_projects_and_links_each() {
    assert_eq!(
        two_projects().render(Kind::Slack, &public_url()).unwrap(),
        r#"{"text":"1 new and 1 regressed issues in 2 projects\n\n[shop] New issue: TypeError: x\nhttps://davy.example/ui/issues/3\n\n[blog] Issue regressed: RangeError: y\nhttps://davy.example/ui/issues/5","unfurl_links":false,"unfurl_media":false}"#
    );
}

#[test]
fn a_digest_of_one_project_names_it_once_and_lists_five_then_links_the_rest() {
    let mut entries = vec![entry(false, "shop", 1, "<!channel> & x")];
    entries.extend((2..=7).map(|id| entry(false, "shop", id, &format!("E{id}"))));
    let digest = Digest {
        id: 1,
        at: 0,
        entries,
    };
    let below = url::Url::parse("https://example.com/davy/").unwrap();
    assert_eq!(
        digest.render(Kind::Slack, &below).unwrap(),
        r#"{"text":"[shop] 7 new issues\n\nNew issue: &lt;!channel&gt; &amp; x\nhttps://example.com/davy/ui/issues/1\n\nNew issue: E2\nhttps://example.com/davy/ui/issues/2\n\nNew issue: E3\nhttps://example.com/davy/ui/issues/3\n\nNew issue: E4\nhttps://example.com/davy/ui/issues/4\n\nNew issue: E5\nhttps://example.com/davy/ui/issues/5\n\nand 2 more: https://example.com/davy/ui/issues","unfurl_links":false,"unfurl_media":false}"#
    );
}

#[test]
fn an_email_digest_has_the_count_as_subject_and_the_oldest_row_in_its_message_id() {
    let mail: Mail =
        serde_json::from_str(&two_projects().render(Kind::Email, &public_url()).unwrap()).unwrap();
    assert_eq!(
        mail,
        Mail {
            subject: "1 new and 1 regressed issues in 2 projects".into(),
            text: "1 new and 1 regressed issues in 2 projects\n\n[shop] New issue: TypeError: x\nhttps://davy.example/ui/issues/3\n\n[blog] Issue regressed: RangeError: y\nhttps://davy.example/ui/issues/5\n".into(),
            message_id: "7.1795164121000".into(),
        }
    );
    let regressed = Digest {
        id: 2,
        at: 0,
        entries: vec![
            entry(true, "shop", 1, "A\r\nBcc: a@b.c"),
            entry(true, "shop", 2, "B"),
        ],
    };
    let mail: Mail =
        serde_json::from_str(&regressed.render(Kind::Email, &public_url()).unwrap()).unwrap();
    assert_eq!(mail.subject, "[shop] 2 regressed issues");
}

fn new_issue(id: i64, title: &str) -> Notice {
    Notice::IssueNew(Issue { id, ..issue(title) })
}

/// `(id, merged, payload)` of each due row at `now`.
fn due(c: &rusqlite::Connection, now: i64) -> Vec<(i64, Vec<i64>, String)> {
    store::due(c, now, &public_url())
        .unwrap()
        .into_iter()
        .map(|d| (d.id, d.merged, d.payload))
        .collect()
}

#[test]
fn issue_notices_wait_30_s_from_the_first_and_go_out_as_one_digest() {
    let c = memory();
    crate::store::create_project(&c, "blog").unwrap();
    channel(&c, None, Kind::Slack);
    store::notify(&c, 1, &new_issue(3, "TypeError: x"), 1_000).unwrap();
    store::notify(&c, 2, &new_issue(4, "RangeError: y"), 5_000).unwrap();
    let regressed = Notice::IssueRegressed(Issue {
        id: 5,
        ..issue("E")
    });
    store::notify(&c, 1, &regressed, 30_999).unwrap();
    assert_eq!(due(&c, 30_999), []);
    store::notify(&c, 1, &new_issue(6, "Late"), 31_000).unwrap();
    assert_eq!(
        due(&c, 31_000),
        [(
            1,
            vec![2, 3],
            r#"{"text":"2 new and 1 regressed issues in 2 projects\n\n[shop] New issue: TypeError: x\nhttps://davy.example/ui/issues/3\n\n[blog] New issue: RangeError: y\nhttps://davy.example/ui/issues/4\n\n[shop] Issue regressed: E\nhttps://davy.example/ui/issues/5","unfurl_links":false,"unfurl_media":false}"#.into()
        )]
    );
    store::delivered(&c, &[1, 2, 3], 31_500, &Ok(())).unwrap();
    assert_eq!(due(&c, 60_999), []);
    assert_eq!(
        due(&c, 61_000),
        [(
            4,
            vec![],
            r#"{"text":"[shop] New issue: Late","unfurl_links":false,"unfurl_media":false}"#.into()
        )]
    );
}

#[test]
fn a_monitor_notice_goes_out_at_once_past_waiting_issue_notices() {
    let c = memory();
    channel(&c, Some(1), Kind::Slack);
    store::notify(&c, 1, &new_issue(3, "TypeError: x"), 1_000).unwrap();
    store::notify(&c, 1, &Notice::MonitorDown(incident()), 2_000).unwrap();
    assert_eq!(
        due(&c, 2_000),
        [(
            2,
            vec![],
            r#"{"text":"[shop] API is down: HTTP 503","unfurl_links":false,"unfurl_media":false}"#
                .into()
        )]
    );
    store::delivered(&c, &[2], 2_100, &Ok(())).unwrap();
    assert_eq!(due(&c, 30_999), []);
    assert_eq!(due(&c, 31_000)[0].0, 1);
}

#[test]
fn a_failed_digest_retries_with_its_rows_and_id_and_a_later_window_waits() {
    let c = memory();
    channel(&c, Some(1), Kind::Webhook);
    store::notify(&c, 1, &new_issue(3, "A"), 0).unwrap();
    store::notify(&c, 1, &new_issue(4, "B"), 1_000).unwrap();
    let first = due(&c, 30_000);
    assert_eq!((first[0].0, first[0].1.clone()), (1, vec![2]));
    store::delivered(&c, &[1, 2], 30_000, &Err("HTTP 500".into())).unwrap();
    store::notify(&c, 1, &new_issue(5, "C"), 40_000).unwrap();
    assert_eq!(due(&c, 59_999), []);
    assert_eq!(due(&c, 60_000), first);
    store::delivered(&c, &[1, 2], 60_000, &Ok(())).unwrap();
    assert_eq!(due(&c, 69_999), []);
    assert_eq!(
        due(&c, 70_000),
        [(
            3,
            vec![],
            r#"{"id":3,"type":"issue.new","project":"shop","at":"1970-01-01T00:00:40Z","issue":{"id":5,"title":"C","culprit":"parseOrder(src/app.js)","level":"error","release":"shop@2.0.0"}}"#.into()
        )]
    );
}
