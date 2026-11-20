use super::*;
use crate::alert::{Destination, EmailAddress};
use crate::monitor::CheckUrl;

fn memory() -> Connection {
    let mut c = Connection::open_in_memory().unwrap();
    crate::store::init(&mut c).unwrap();
    crate::store::create_project(&c, "shop").unwrap();
    c
}

fn item(client: Option<&str>) -> Feedback {
    Feedback::new(
        client,
        "search_logs with since=yesterday".to_owned(),
        "unknown duration \"yesterday\"".to_owned(),
        Some("accept yesterday and today".to_owned()),
    )
    .unwrap()
}

#[test]
fn new_refuses_empty_and_long_text_and_cleans_the_client_name() {
    let new = |tried: &str, happened: &str, fix: Option<&str>| {
        Feedback::new(
            Some("claude-code\u{1b}[2J"),
            tried.to_owned(),
            happened.to_owned(),
            fix.map(str::to_owned),
        )
    };
    assert_eq!(new(" ", "x", None), Err("tried is empty".to_owned()));
    assert_eq!(new("x", "", None), Err("happened is empty".to_owned()));
    assert_eq!(
        new("x", "y", Some(&"é".repeat(4001))),
        Err("fix holds 4001 characters and may hold 4000, shorten it".to_owned())
    );
    assert_eq!(
        new("x", "y", Some("  ")),
        Ok(Feedback {
            client: Some("claude-code[2J".to_owned()),
            tried: "x".to_owned(),
            happened: "y".to_owned(),
            fix: None,
        })
    );
    let long = Feedback::new(Some(&"a".repeat(300)), "x".into(), "y".into(), None).unwrap();
    assert_eq!(long.client.map(|c| c.len()), Some(100));
    let blank = Feedback::new(Some("\n"), "x".into(), "y".into(), None).unwrap();
    assert_eq!(blank.client, None);
}

#[test]
fn feedback_is_listed_oldest_first_and_only_the_newest_1000_are_kept() {
    let c = memory();
    assert_eq!(add(&c, &item(Some("codex")), 1_000).unwrap(), 1);
    assert_eq!(
        list(&c).unwrap(),
        [Listed {
            id: 1,
            at: "1970-01-01T00:00:01Z".to_owned(),
            feedback: item(Some("codex")),
        }]
    );
    for at in 1..=1000 {
        add(&c, &item(None), 1_000 + at).unwrap();
    }
    let ids: Vec<i64> = list(&c).unwrap().iter().map(|l| l.id).collect();
    assert_eq!(ids.len(), 1000);
    assert_eq!((ids[0], ids[999]), (2, 1001));
}

#[test]
fn only_channels_added_with_feedback_get_a_notice() {
    let c = memory();
    let hook = CheckUrl::parse("https://hooks.example/x").unwrap();
    let add_channel =
        |to: Destination, feedback| alert::store::add(&c, Some(1), &to, feedback, 0).unwrap().id;
    let webhook = add_channel(Destination::Webhook(hook.clone()), true);
    add_channel(Destination::Webhook(hook.clone()), false);
    let slack = add_channel(Destination::Slack(hook), true);
    let email = add_channel(
        Destination::Email(EmailAddress::parse("ops@example.com").unwrap()),
        true,
    );
    add(&c, &item(Some("claude-code")), 2_000).unwrap();
    let rows: Vec<(i64, String, String)> = c
        .prepare("SELECT channel_id,type,payload FROM notifications ORDER BY id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let feedback = "feedback".to_owned();
    assert_eq!(
        rows,
        [
            (
                webhook,
                feedback.clone(),
                r#"{"id":1,"type":"feedback","project":null,"at":"1970-01-01T00:00:02Z","feedback":{"id":1,"client":"claude-code","tried":"search_logs with since=yesterday","happened":"unknown duration \"yesterday\"","fix":"accept yesterday and today"}}"#.to_owned()
            ),
            (
                slack,
                feedback.clone(),
                r#"{"text":"Feedback from claude-code: unknown duration \"yesterday\""}"#.to_owned()
            ),
            (
                email,
                feedback,
                r#"{"subject":"Feedback from claude-code: unknown duration \"yesterday\"","text":"Feedback from claude-code: unknown duration \"yesterday\"\n\nfeedback_id: 1\nclient: claude-code\ntried: search_logs with since=yesterday\nhappened: unknown duration \"yesterday\"\nfix: accept yesterday and today\nat: 1970-01-01T00:00:02Z\n","message_id":"3.2000"}"#.to_owned()
            ),
        ]
    );
}

#[test]
fn the_cli_prints_one_line_per_field_without_control_characters() {
    let data = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("target/test-feedback-{}", uuid::Uuid::new_v4()));
    let c = crate::store::open(&data).unwrap();
    let tricky = Feedback::new(
        None,
        "sql\nline two".to_owned(),
        "\u{1b}[1Aboom".to_owned(),
        None,
    )
    .unwrap();
    add(&c, &tricky, 3_000).unwrap();
    add(&c, &item(Some("codex")), 4_000).unwrap();
    let run_to_string = |json| {
        let mut out = Vec::new();
        run(
            &mut out,
            &Args {
                data: data.clone(),
                json,
            },
        )
        .unwrap();
        String::from_utf8(out).unwrap()
    };
    assert_eq!(
        run_to_string(false),
        "1\t1970-01-01T00:00:03Z\t-
  tried: sql line two
  happened:  [1Aboom
2\t1970-01-01T00:00:04Z\tcodex
  tried: search_logs with since=yesterday
  happened: unknown duration \"yesterday\"
  fix: accept yesterday and today
"
    );
    assert_eq!(
        run_to_string(true),
        r#"[{"id":1,"at":"1970-01-01T00:00:03Z","client":null,"tried":"sql\nline two","happened":"\u001b[1Aboom","fix":null},{"id":2,"at":"1970-01-01T00:00:04Z","client":"codex","tried":"search_logs with since=yesterday","happened":"unknown duration \"yesterday\"","fix":"accept yesterday and today"}]
"#
    );
}
