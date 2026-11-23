use super::checkin::{
    CheckIn, CheckInId, Config, Dropped, MAX_ITEM, MonitorConfig, MonitorSlug, Status, parse,
};
use super::schedule::{Crontab, Schedule, Zone};
use crate::envelope::{Envelope, ItemKind};
use std::time::Duration;

/// The check-in item of envelope `n` that `sdk` sent, from
/// `e2e/fixtures/<sdk>-checkins/`.
fn captured(sdk: &str, n: u8) -> Vec<u8> {
    let body = crate::test_support::fixture(&format!("{sdk}-checkins/{n}.body"));
    let envelope = Envelope::parse(&body).unwrap();
    let [item] = &envelope.items[..] else {
        panic!("one item in {sdk} {n}")
    };
    assert_eq!(item.kind, ItemKind::CheckIn);
    item.payload.to_vec()
}

fn slug(s: &str) -> MonitorSlug {
    MonitorSlug::parse(s).unwrap()
}

fn id(s: &str) -> Option<CheckInId> {
    CheckInId::parse(s)
}

/// The configs of the three monitors in `e2e/capture-checkins.mjs`.
fn captured_configs() -> [MonitorConfig; 3] {
    let hourly = MonitorConfig {
        schedule: Schedule::Cron {
            spec: Crontab::parse("0 * * * *").unwrap(),
            zone: Zone::parse("Europe/Prague").unwrap(),
        },
        margin: Duration::from_mins(2),
        max_runtime: Duration::from_mins(10),
    };
    let two_hours = MonitorConfig {
        schedule: Schedule::Every(Duration::from_hours(2)),
        margin: Duration::from_mins(1),
        max_runtime: Duration::from_mins(30),
    };
    let half_hour = MonitorConfig {
        schedule: Schedule::Every(Duration::from_mins(30)),
        margin: Duration::from_mins(1),
        max_runtime: Duration::from_mins(5),
    };
    [hourly, two_hours, half_hour]
}

#[test]
fn sentry_node_10_check_ins() {
    let [hourly, two_hours, half_hour] = captured_configs();
    let check_in = |s, status, i, duration, config| CheckIn {
        slug: slug(s),
        status,
        id: id(i),
        duration,
        config,
    };
    let nightly = "9bcd4f844e1344919408cda90724bc82";
    let feed = "4c6f05a21b444656a4ae604d69d3e1f1";
    let billing = "2a31a644c40041b9942df6d115f2519a";
    let want = [
        check_in(
            "nightly-report",
            Status::InProgress,
            nightly,
            None,
            Config::Valid(hourly),
        ),
        check_in(
            "nightly-report",
            Status::Ok,
            nightly,
            Some(Duration::from_secs_f64(0.000_031_471_252_441_406_25)),
            Config::Absent,
        ),
        check_in(
            "import-feed",
            Status::InProgress,
            feed,
            None,
            Config::Valid(two_hours),
        ),
        check_in(
            "import-feed",
            Status::Error,
            feed,
            Some(Duration::from_secs_f64(0.000_052_690_505_981_445_31)),
            Config::Absent,
        ),
        check_in(
            "billing-sync",
            Status::InProgress,
            billing,
            None,
            Config::Valid(half_hour),
        ),
        check_in(
            "billing-sync",
            Status::Ok,
            billing,
            Some(Duration::from_millis(12_500)),
            Config::Absent,
        ),
        check_in(
            "billing-sync",
            Status::Error,
            "ebb5cb198cf240999abe330fafee09fa",
            None,
            Config::Absent,
        ),
    ];
    for (n, want) in (1..).zip(want) {
        assert_eq!(
            parse(&captured("sentry-node-10", n)),
            Ok(want),
            "envelope {n}"
        );
    }
}

#[test]
fn sentry_node_7_sends_the_same_shape() {
    let want = [
        ("nightly-report", Status::InProgress, true),
        ("nightly-report", Status::Ok, false),
        ("import-feed", Status::InProgress, true),
        ("import-feed", Status::Error, false),
        ("billing-sync", Status::InProgress, true),
        ("billing-sync", Status::Ok, false),
        ("billing-sync", Status::Error, false),
    ];
    for (n, (s, status, config)) in (1..).zip(want) {
        let got = parse(&captured("sentry-node-7", n)).unwrap();
        let from_10 = parse(&captured("sentry-node-10", n)).unwrap();
        assert_eq!(
            (got.slug, got.status, got.id.is_some()),
            (slug(s), status, true),
            "envelope {n}"
        );
        assert_eq!(
            matches!(got.config, Config::Valid(_)),
            config,
            "envelope {n}"
        );
        assert_eq!(got.config, from_10.config, "envelope {n}");
    }
}

fn item(json: &str) -> Result<CheckIn, Dropped> {
    parse(json.as_bytes())
}

fn config_of(monitor_config: &str) -> Config {
    let json =
        format!(r#"{{"monitor_slug":"job","status":"ok","monitor_config":{monitor_config}}}"#);
    item(&json).unwrap().config
}

#[test]
fn refused_items_say_why() {
    let big = format!(
        r#"{{"monitor_slug":"job","status":"ok","pad":"{}"}}"#,
        "x".repeat(MAX_ITEM)
    );
    let cases = [
        (big.as_str(), Dropped::TooLarge),
        ("", Dropped::Invalid),
        ("[]", Dropped::Invalid),
        (r#"{"monitor_slug":"job"}"#, Dropped::Invalid),
        (
            r#"{"monitor_slug":"job","status":"missed"}"#,
            Dropped::Invalid,
        ),
        (r#"{"monitor_slug":"job","status":"OK"}"#, Dropped::Invalid),
        (
            r#"{"monitor_slug":"job","status":"ok","duration":"5"}"#,
            Dropped::Invalid,
        ),
        (r#"{"status":"ok"}"#, Dropped::Invalid),
        (r#"{"monitor_slug":"","status":"ok"}"#, Dropped::BadSlug),
        (
            r#"{"monitor_slug":"my.job","status":"ok"}"#,
            Dropped::BadSlug,
        ),
        (
            r#"{"monitor_slug":"job\u001b[31m","status":"ok"}"#,
            Dropped::BadSlug,
        ),
        (r#"{"monitor_slug":"ä","status":"ok"}"#, Dropped::BadSlug),
    ];
    for (json, why) in cases {
        assert_eq!(item(json), Err(why), "{json:.80}");
    }
    let long = format!(r#"{{"monitor_slug":"{}","status":"ok"}}"#, "a".repeat(51));
    assert_eq!(item(&long), Err(Dropped::BadSlug));
    let longest = format!(r#"{{"monitor_slug":"{}","status":"ok"}}"#, "A".repeat(50));
    assert_eq!(item(&longest).unwrap().slug.as_str(), "a".repeat(50));
}

#[test]
fn ids_and_durations() {
    let parsed = |extra: &str| {
        item(&format!(
            r#"{{"monitor_slug":"Nightly_Report-2","status":"ok"{extra}}}"#
        ))
        .unwrap()
    };
    assert_eq!(parsed("").slug.as_str(), "nightly_report-2");
    let cases = [
        (
            r#","check_in_id":"9BCD4F84-4E13-4491-9408-CDA90724BC82""#,
            Some("9bcd4f844e1344919408cda90724bc82"),
        ),
        (r#","check_in_id":"00000000000000000000000000000000""#, None),
        (
            r#","check_in_id":"00000000-0000-0000-0000-000000000000""#,
            None,
        ),
        (r#","check_in_id":"not-an-id""#, None),
        (r#","check_in_id":null"#, None),
        ("", None),
    ];
    for (extra, want) in cases {
        assert_eq!(
            parsed(extra).id.as_ref().map(CheckInId::as_str),
            want,
            "{extra}"
        );
    }
    let durations = [
        (r#","duration":0"#, Some(Duration::ZERO)),
        (r#","duration":604800"#, Some(Duration::from_hours(168))),
        (r#","duration":604800.5"#, None),
        (r#","duration":-1"#, None),
        (r#","duration":null"#, None),
    ];
    for (extra, want) in durations {
        assert_eq!(parsed(extra).duration, want, "{extra}");
    }
}

#[test]
fn configs_are_bounded() {
    let every = |d, margin, runtime| {
        Config::Valid(MonitorConfig {
            schedule: Schedule::Every(d),
            margin: Duration::from_mins(margin),
            max_runtime: Duration::from_mins(runtime),
        })
    };
    let hour = Duration::from_hours(1);
    let interval = r#""schedule":{"type":"interval","value":1,"unit":"hour"}"#;
    let cases = [
        (format!("{{{interval}}}"), every(hour, 1, 30)),
        (
            format!(r#"{{{interval},"checkin_margin":0,"max_runtime":1}}"#),
            every(hour, 0, 1),
        ),
        (
            format!(r#"{{{interval},"checkin_margin":5000,"max_runtime":99999}}"#),
            every(hour, 1440, 10_080),
        ),
        (
            format!(r#"{{{interval},"checkin_margin":-3,"max_runtime":0}}"#),
            every(hour, 1, 1),
        ),
        (
            format!(r#"{{{interval},"checkin_margin":null,"timezone":"UTC"}}"#),
            every(hour, 1, 30),
        ),
    ];
    for (json, want) in cases {
        assert_eq!(config_of(&json), want, "{json}");
    }
    let months = config_of(
        r#"{"schedule":{"type":"interval","value":1,"unit":"year"},"timezone":"america/new_york"}"#,
    );
    let Config::Valid(MonitorConfig {
        schedule: Schedule::Months { n: 12, zone },
        ..
    }) = months
    else {
        panic!("{months:?}")
    };
    assert_eq!(zone.name(), "America/New_York");
    assert_eq!(config_of("null"), Config::Absent);
    for bad in [
        "{}",
        "[1]",
        r#"{"schedule":{"type":"crontab","value":"0 0 31 2 *"}}"#,
        r#"{"schedule":{"type":"crontab","value":5}}"#,
        r#"{"schedule":{"type":"crontab","value":"0 * * * *"},"timezone":"Mars/Base"}"#,
        r#"{"schedule":{"type":"interval","value":"2","unit":"hour"}}"#,
        r#"{"schedule":{"type":"interval","value":2}}"#,
        r#"{"schedule":{"type":"interval","value":2.5,"unit":"hour"}}"#,
        r#"{"schedule":{"type":"interval","value":-2,"unit":"hour"}}"#,
        r#"{"schedule":{"type":"interval","value":30,"unit":"second"}}"#,
        r#"{"schedule":{"type":"interval","value":[1,2,3],"unit":"hour"}}"#,
        r#"{"schedule":{"type":"interval","value":1,"unit":"hour"},"checkin_margin":"5"}"#,
        r#"{"schedule":{"type":"solar","value":1}}"#,
    ] {
        assert_eq!(config_of(bad), Config::Invalid, "{bad}");
    }
}
