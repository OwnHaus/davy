use super::*;

const DAY: Timestamp = 20_000 * DAY_MS;
const EVENT: &str = "0123456789abcdef0123456789abcdef";

fn memory() -> Connection {
    let mut c = Connection::open_in_memory().unwrap();
    crate::store::init(&mut c).unwrap();
    crate::store::create_project(&c, "shop").unwrap();
    crate::store::create_project(&c, "admin").unwrap();
    c.execute_batch(&format!(
        "INSERT INTO issues(id,project_id,grouping_version,fingerprint,title,level,status,first_seen,last_seen,event_count)
         VALUES (7,1,1,'fp','TypeError: x is undefined','error','unresolved',0,0,1);
         INSERT INTO events(project_id,issue_id,event_id,timestamp,received_at,level) VALUES (1,7,'{EVENT}',0,0,'error');"
    ))
    .unwrap();
    c
}

fn said(message: &str, event: Option<&str>) -> UserFeedback {
    UserFeedback {
        event_id: event.map(|e| EventId(e.to_owned())),
        message: message.to_owned(),
        name: None,
        email: None,
        url: None,
    }
}

#[test]
fn parse_feedback_reads_the_feedback_context_of_a_feedback_event() {
    let payload = br#"{"type": "feedback", "event_id": "ffffffffffffffffffffffffffffffff",
        "contexts": {"feedback": {"message": "  The pay button does nothing\n",
        "contact_email": "ana@example.com", "name": "Ana", "url": "https://shop.example.com/cart",
        "source": "api", "associated_event_id": "01234567-89ab-cdef-0123-456789abcdef"}}}"#;
    assert_eq!(
        parse_feedback(payload),
        Some(UserFeedback {
            event_id: Some(EventId(EVENT.to_owned())),
            message: "The pay button does nothing".to_owned(),
            name: Some("Ana".to_owned()),
            email: Some("ana@example.com".to_owned()),
            url: Some("https://shop.example.com/cart".to_owned()),
        })
    );
}

#[test]
fn parse_user_report_reads_the_old_item() {
    let payload = format!(
        r#"{{"event_id": "{EVENT}", "comments": "It crashed\u001b[2J again", "name": " ", "email": "bo@example.com"}}"#
    );
    assert_eq!(
        parse_user_report(payload.as_bytes()),
        Some(UserFeedback {
            event_id: Some(EventId(EVENT.to_owned())),
            message: "It crashed [2J again".to_owned(),
            name: None,
            email: Some("bo@example.com".to_owned()),
            url: None,
        })
    );
}

#[test]
fn parse_refuses_feedback_without_a_message_and_drops_a_bad_event_id() {
    assert_eq!(
        parse_feedback(br#"{"contexts": {"feedback": {"message": " \n "}}}"#),
        None
    );
    assert_eq!(parse_feedback(br#"{"contexts": {}}"#), None);
    assert_eq!(parse_user_report(b"[]"), None);
    assert_eq!(
        parse_feedback(
            br#"{"contexts": {"feedback": {"message": "hi", "associated_event_id": "nope"}}}"#
        ),
        Some(said("hi", None))
    );
}

#[test]
fn parse_cuts_long_fields_by_characters() {
    let payload = format!(
        r#"{{"contexts": {{"feedback": {{"message": "{}", "name": "{}"}}}}}}"#,
        "ž".repeat(5000),
        "n".repeat(200)
    );
    let f = parse_feedback(payload.as_bytes()).unwrap();
    assert_eq!(f.message.chars().count(), 4096);
    assert_eq!(f.name.unwrap().len(), 128);
}

#[test]
fn add_links_feedback_to_the_issue_of_its_event_in_the_same_project() {
    let c = memory();
    assert!(add(&c, 1, DAY, &said("first", Some(EVENT))).unwrap());
    assert!(add(&c, 1, DAY + 1, &said("no event", None)).unwrap());
    assert!(add(&c, 2, DAY + 2, &said("other project", Some(EVENT))).unwrap());
    assert!(add(&c, 1, DAY + 3, &said("second", Some(EVENT))).unwrap());
    let messages: Vec<String> = of_issue(&c, 7, 10)
        .unwrap()
        .into_iter()
        .map(|f| f.message)
        .collect();
    assert_eq!(messages, vec!["second", "first"]);
    assert_eq!(of_issue(&c, 7, 1).unwrap().len(), 1);
}

#[test]
fn of_issue_returns_what_an_agent_reads() {
    let c = memory();
    let f = UserFeedback {
        name: Some("Ana".to_owned()),
        email: Some("ana@example.com".to_owned()),
        url: Some("https://shop.example.com/cart".to_owned()),
        ..said("The pay button does nothing", Some(EVENT))
    };
    add(&c, 1, 1_700_000_000_123, &f).unwrap();
    assert_eq!(
        of_issue(&c, 7, 10).unwrap(),
        vec![IssueFeedback {
            received_at: "2023-11-14T22:13:20.123Z".to_owned(),
            message: "The pay button does nothing".to_owned(),
            name: Some("Ana".to_owned()),
            email: Some("ana@example.com".to_owned()),
            url: Some("https://shop.example.com/cart".to_owned()),
            event_id: EVENT.to_owned(),
        }]
    );
}

#[test]
fn add_refuses_past_the_daily_cap_until_the_next_utc_day() {
    let c = memory();
    for i in 0..MAX_PER_DAY {
        assert!(add(&c, 1, DAY + i, &said("spam", None)).unwrap());
    }
    assert!(!add(&c, 1, DAY + DAY_MS - 1, &said("one more", None)).unwrap());
    assert!(add(&c, 2, DAY + 5, &said("other project", None)).unwrap());
    assert!(add(&c, 1, DAY + DAY_MS, &said("next day", None)).unwrap());
}

#[test]
fn items_add_counts_feedback_past_the_cap_as_a_client_report() {
    let c = memory();
    let over: Vec<crate::items::SdkItem> = (0..MAX_PER_DAY + 2)
        .map(|_| crate::items::SdkItem::UserFeedback(said("spam", None)))
        .collect();
    crate::items::add(&c, 1, DAY, &over).unwrap();
    let (reason, category, quantity): (String, String, i64) = c
        .query_row(
            "SELECT reason, category, quantity FROM client_reports",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        (reason.as_str(), category.as_str(), quantity),
        ("davy_daily_cap", "feedback", 2)
    );
}
