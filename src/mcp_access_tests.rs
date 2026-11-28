//! The levels of agent tokens on `/mcp`, against a real store writer.

use super::super::{Tools, router};
use crate::{
    access::{Level, need},
    drops::{self, By, Spec},
    retention::{self, Defaults},
    store, time,
    token::Token,
    tokens::{self, Change, Name},
    usage,
};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::header,
};
use serde_json::{Value, json};
use std::path::PathBuf;
use tower::ServiceExt;

const ADMIN: &str = "a-test-token-that-is-long-enough!";

const READ_TOOLS: [&str; 12] = [
    "connect_project",
    "count_logs",
    "drop_rules",
    "issue_context",
    "ledger",
    "log_patterns",
    "project_status",
    "release_health",
    "schema",
    "search_logs",
    "sql",
    "usage",
];
const TRIAGE_TOOLS: [&str; 4] = [
    "drop_rule_remove",
    "drop_rule_shadow",
    "mark_fixed",
    "submit_feedback",
];

struct Server {
    app: Router,
    data: PathBuf,
    /// Applies the jobs, as in `davy serve`.
    writer: store::Writer,
}

/// A store with project `api` and a writer, `/mcp` with the admin token
/// `admin`.
fn server(admin: Option<&str>) -> Server {
    let data = crate::test_support::scratch("mcp-access");
    let c = store::open(&data).unwrap();
    store::create_project(&c, "api").unwrap();
    let writer = store::Writer::start(store::open(&data).unwrap(), 64);
    let app = router(
        data.clone(),
        writer.sender.clone(),
        admin.map(Token::new),
        &url::Url::parse("https://davy.example/").unwrap(),
        Defaults::default(),
        usage::Shared::default(),
    )
    .unwrap();
    Server { app, data, writer }
}

impl Server {
    /// Adds token `name` at `level`, unlocked for destructive changes
    /// with `unlocked`, and returns its secret.
    fn token(&self, name: &str, level: Level, unlocked: bool) -> String {
        let c = store::open(&self.data).unwrap();
        let name = Name::parse(name).unwrap();
        let (_, secret) = tokens::create(&c, &name, level, time::now_ms())
            .unwrap()
            .unwrap();
        if unlocked {
            let unlock = Change::Unlock {
                name: name.as_str().into(),
                by: "ann@example.com".into(),
                at: time::now_ms(),
            };
            tokens::apply(&c, &unlock).unwrap().unwrap();
        }
        secret
    }

    async fn post(&self, token: Option<&str>, body: Value) -> (u16, Value) {
        let mut req = axum::http::Request::post("/mcp")
            .header(header::HOST, "localhost")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json, text/event-stream");
        if let Some(t) = token {
            req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
        }
        let r = self
            .app
            .clone()
            .oneshot(req.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = r.status().as_u16();
        let body = to_bytes(r.into_body(), 1_000_000).await.unwrap();
        let body = serde_json::from_slice(&body).unwrap_or(Value::Null);
        (status, body)
    }

    async fn tools(&self, token: &str) -> Vec<String> {
        let list = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"});
        let (status, body) = self.post(Some(token), list).await;
        assert_eq!(status, 200, "{body}");
        let tools = body["result"]["tools"].as_array().unwrap();
        tools
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_owned())
            .collect()
    }

    /// The text and `isError` flag of a tool result.
    async fn call(&self, token: &str, tool: &str, arguments: Value) -> (String, bool) {
        let call = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                          "params": {"name": tool, "arguments": arguments}});
        let (status, body) = self.post(Some(token), call).await;
        assert_eq!(status, 200, "{body}");
        let result = &body["result"];
        (
            result["content"][0]["text"].as_str().unwrap().to_owned(),
            result["isError"].as_bool().unwrap_or(false),
        )
    }

    fn logs_days(&self) -> u32 {
        let c = store::open(&self.data).unwrap();
        let kept = retention::one(&c, "api", Defaults::default(), time::now_ms());
        kept.unwrap().unwrap().logs.days.get()
    }
}

fn sorted(names: &[&[&str]]) -> Vec<String> {
    let mut all: Vec<String> = names
        .iter()
        .flat_map(|n| n.iter())
        .map(|n| (*n).to_owned())
        .collect();
    all.sort();
    all
}

#[test]
fn the_table_classifies_every_tool_and_no_other_name() {
    let mut names: Vec<String> =
        (Tools::tool_router() + Tools::drop_router() + Tools::ledger_router())
            .list_all()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect();
    names.sort();
    let all = sorted(&[
        &READ_TOOLS,
        &TRIAGE_TOOLS,
        &["set_retention", "drop_rule_live"],
    ]);
    assert_eq!(names, all);
    for name in &names {
        assert!(need(name).is_some(), "{name} has no need");
    }
}

#[tokio::test]
async fn each_token_lists_the_tools_its_level_reaches() {
    let s = server(Some(ADMIN));
    let read = s.token("reader", Level::Read, false);
    let triage = s.token("triager", Level::Triage, false);
    let full = s.token("ops", Level::Full, false);
    let unlocked = s.token("trusted", Level::Full, true);
    assert_eq!(s.tools(&read).await, sorted(&[&READ_TOOLS]));
    assert_eq!(
        s.tools(&triage).await,
        sorted(&[&READ_TOOLS, &TRIAGE_TOOLS])
    );
    let full_tools = sorted(&[&READ_TOOLS, &TRIAGE_TOOLS, &["set_retention"]]);
    assert_eq!(s.tools(&full).await, full_tools);
    assert_eq!(s.tools(ADMIN).await, full_tools);
    let all = sorted(&[
        &READ_TOOLS,
        &TRIAGE_TOOLS,
        &["set_retention", "drop_rule_live"],
    ]);
    assert_eq!(s.tools(&unlocked).await, all);
}

#[tokio::test]
async fn a_call_above_the_level_is_refused_and_names_the_level() {
    let s = server(Some(ADMIN));
    let read = s.token("reader", Level::Read, false);
    let (text, error) = s.call(&read, "mark_fixed", json!({"issue_id": 1})).await;
    assert!(error);
    assert_eq!(
        text,
        "mark_fixed needs level triage, and the token \"reader\" holds read. A person changes a token's level, and unlocks destructive changes, on the Tokens page of the web UI or with davy token."
    );
    let (text, error) = s.call(&read, "schema", json!({})).await;
    assert!(!error, "{text}");
    let unknown = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                         "params": {"name": "delete_everything", "arguments": {}}});
    let (status, body) = s.post(Some(&read), unknown).await;
    assert_eq!(status, 200);
    assert_eq!(body["error"]["message"], "tool not found");
}

#[tokio::test]
async fn shortening_a_retention_needs_the_unlock_and_keeping_longer_does_not() {
    let s = server(Some(ADMIN));
    let full = s.token("ops", Level::Full, false);
    let unlocked = s.token("trusted", Level::Full, true);
    let (text, error) = s
        .call(
            &full,
            "set_retention",
            json!({"project": "api", "logs_days": 60}),
        )
        .await;
    assert!(!error, "{text}");
    assert_eq!(s.logs_days(), 60);
    let why = "an agent may only keep data longer: logs are kept 60 days and 7 would delete what is older for good. A person shortens it with `davy project retention` or in the web UI.";
    let (text, error) = s
        .call(
            &full,
            "set_retention",
            json!({"project": "api", "logs_days": 7}),
        )
        .await;
    assert_eq!(
        (text, error),
        (
            format!(
                "{why} set_retention needs level full with destructive changes unlocked, and the token \"ops\" holds full. A person changes a token's level, and unlocks destructive changes, on the Tokens page of the web UI or with davy token."
            ),
            true
        )
    );
    let (text, error) = s
        .call(
            ADMIN,
            "set_retention",
            json!({"project": "api", "logs_days": 7}),
        )
        .await;
    assert!(error && text.starts_with(why), "{text}");
    assert_eq!(s.logs_days(), 60);
    let (text, error) = s
        .call(
            &unlocked,
            "set_retention",
            json!({"project": "api", "logs_days": 7}),
        )
        .await;
    assert!(!error, "{text}");
    assert_eq!(s.logs_days(), 7);
}

#[tokio::test]
async fn an_unlocked_token_never_shortens_davys_own_retention() {
    let s = server(Some(ADMIN));
    let mut c = store::open(&s.data).unwrap();
    let (own, _) = crate::self_report::own::find_or_create(&mut c).unwrap();
    let unlocked = s.token("trusted", Level::Full, true);
    let shorter = json!({"project": own.slug, "logs_days": 1, "errors_days": 1});
    let (text, error) = s.call(&unlocked, "set_retention", shorter).await;
    assert_eq!(
        (text.as_str(), error),
        (
            "project \"davy\" is Davy's own, whose warnings and errors davy doctor and project_status read: an agent never shortens its retention, a person can with `davy project retention`",
            true
        )
    );
    let kept = retention::one(&c, &own.slug, Defaults::default(), time::now_ms());
    let kept = kept.unwrap().unwrap();
    assert_eq!(
        (kept.logs.days.get(), kept.errors.days.get()),
        (
            Defaults::default().logs.get(),
            Defaults::default().errors.get()
        )
    );
}

#[tokio::test]
async fn set_retention_refuses_an_argument_it_does_not_know() {
    let s = server(Some(ADMIN));
    let full = s.token("ops", Level::Full, false);
    let args = json!({"project": "api", "logs_days": 60, "note": "x"});
    let (text, error) = s.call(&full, "set_retention", args).await;
    assert!(error && text.contains("unknown field `note`"), "{text}");
    assert_eq!(s.logs_days(), Defaults::default().logs.get());
}

#[tokio::test]
async fn only_an_unlocked_token_makes_a_drop_rule_live() {
    let s = server(Some(ADMIN));
    let c = store::open(&s.data).unwrap();
    let project = store::project_by_slug(&c, "api").unwrap();
    let spec = Spec::new(None, None, Some("noise"), None, None).unwrap();
    let rule = drops::add(&c, project, &spec, false, By::Agent, time::now_ms())
        .unwrap()
        .unwrap()
        .id;
    let args = json!({"project": "api", "rule_id": rule});
    let full = s.token("ops", Level::Full, false);
    let (text, error) = s.call(&full, "drop_rule_live", args.clone()).await;
    assert!(error);
    assert_eq!(
        text,
        "drop_rule_live needs level full with destructive changes unlocked, and the token \"ops\" holds full. A person changes a token's level, and unlocks destructive changes, on the Tokens page of the web UI or with davy token."
    );
    assert!(!drops::get(&c, rule).unwrap().unwrap().live);
    let unlocked = s.token("trusted", Level::Full, true);
    let (text, error) = s.call(&unlocked, "drop_rule_live", args.clone()).await;
    assert!(!error, "{text}");
    let stored = drops::get(&c, rule).unwrap().unwrap();
    assert_eq!(
        (stored.live, stored.by, stored.expires_at),
        (true, By::Person, None)
    );
    let (text, error) = s.call(&unlocked, "drop_rule_live", args).await;
    assert!(error);
    assert_eq!(text, format!("rule {rule} is live already"));
}

#[tokio::test]
async fn an_unlocked_change_and_its_row_are_one_write() {
    let s = server(Some(ADMIN));
    let mut c = store::open(&s.data).unwrap();
    let (own, _) = crate::self_report::own::find_or_create(&mut c).unwrap();
    let project = store::project_by_slug(&c, "api").unwrap();
    let spec = Spec::new(None, None, Some("noise"), None, None).unwrap();
    let rule = drops::add(&c, project, &spec, false, By::Agent, time::now_ms())
        .unwrap()
        .unwrap()
        .id;
    let unlocked = s.token("trusted", Level::Full, true);
    let live = json!({"project": "api", "rule_id": rule});
    let shorter = |project: &str| json!({"project": project, "logs_days": 7});
    for (tool, args) in [
        ("drop_rule_live", live.clone()),
        ("drop_rule_live", live),
        ("set_retention", shorter("api")),
        ("set_retention", shorter(&own.slug)),
        ("set_retention", json!({"project": "api", "logs_days": 9})),
    ] {
        s.call(&unlocked, tool, args).await;
    }
    let mut q = c
        .prepare("SELECT action, outcome, destructive, summary LIKE '%; refused' FROM ledger ORDER BY id")
        .unwrap();
    let rows: Vec<(String, String, bool, bool)> = q
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let row = |action: &str, outcome: &str, destructive, refused| {
        (action.to_owned(), outcome.to_owned(), destructive, refused)
    };
    assert_eq!(
        rows,
        [
            row("drop_rule_live", "done", true, false),
            row("drop_rule_live", "error", false, true),
            row("set_retention", "done", true, false),
            row("set_retention", "error", false, true),
            row("set_retention", "done", false, false),
        ]
    );
}

#[tokio::test]
async fn a_revoked_or_made_up_token_gets_401_and_agent_tokens_work_without_an_admin_token() {
    let s = server(None);
    let list = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"});
    let read = s.token("reader", Level::Read, false);
    assert_eq!(s.post(Some(&read), list.clone()).await.0, 200);
    for token in [None, Some(ADMIN), Some("davy_agent_made-up"), Some("")] {
        assert_eq!(s.post(token, list.clone()).await.0, 401, "{token:?}");
    }
    let c = store::open(&s.data).unwrap();
    let revoke = Change::Revoke {
        name: "reader".into(),
        at: time::now_ms(),
    };
    tokens::apply(&c, &revoke).unwrap().unwrap();
    assert_eq!(s.post(Some(&read), list).await.0, 401);
}

#[tokio::test]
async fn a_call_records_when_the_token_was_last_used() {
    let s = server(None);
    let read = s.token("reader", Level::Read, false);
    s.call(&read, "schema", json!({})).await;
    // The writer applies jobs in order, so once a later one is done the
    // call's touch is stored.
    let (done, applied) = tokio::sync::oneshot::channel();
    let job = store::Job::Token(Change::Used { id: 0, at: 0 });
    s.writer.sender.send(store::Write { job, done }).unwrap();
    applied.await.unwrap().unwrap();
    let c = store::open(&s.data).unwrap();
    let used_at = tokens::one(&c, "reader").unwrap().unwrap().used_at;
    assert!(used_at.is_some());
}

#[tokio::test]
async fn changes_and_refusals_leave_a_row_and_reads_do_not() {
    let s = server(Some(ADMIN));
    let read = s.token("reader", Level::Read, false);
    let triage = s.token("triager", Level::Triage, false);
    for _ in 0..2 {
        let (_, error) = s.call(&read, "mark_fixed", json!({"issue_id": 99})).await;
        assert!(error);
    }
    s.call(&read, "schema", json!({})).await;
    let feedback = json!({"tried": "a secret text", "happened": "nothing"});
    let (text, error) = s.call(&triage, "submit_feedback", feedback).await;
    assert!(!error, "{text}");
    let (_, error) = s.call(&triage, "mark_fixed", json!({"issue_id": 99})).await;
    assert!(error);
    let call = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                      "params": {"name": "submit_feedback", "arguments": {"tried": 7, "happened": "a secret text"}}});
    s.post(Some(&triage), call).await;
    let (text, error) = s.call(&read, "ledger", json!({"limit": 3})).await;
    assert!(!error, "{text}");
    let page: Value = serde_json::from_str(&text).unwrap();
    assert!(!text.contains("a secret text"), "{text}");
    let triager =
        |action, outcome, summary: String| json!(["triager", action, outcome, summary, 1]);
    let feedback = "sent feedback about Davy";
    assert_eq!(
        brief(&page),
        [
            triager(
                "submit_feedback",
                "error",
                format!("{feedback}; invalid arguments")
            ),
            triager(
                "mark_fixed",
                "error",
                "resolved, fixed in the next release; tool error".into()
            ),
            triager("submit_feedback", "done", feedback.into()),
        ]
    );
    let before = page["next_before"].as_i64().unwrap();
    let (text, _) = s.call(&read, "ledger", json!({"before": before})).await;
    let page: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        brief(&page),
        [json!([
            "reader",
            "mark_fixed",
            "denied",
            "resolved, fixed in the next release; needs triage, holds read",
            2
        ])]
    );
    assert_eq!(
        (
            &page["rows"][0]["actor_kind"],
            &page["rows"][0]["issue"],
            &page["next_before"]
        ),
        (&json!("token"), &json!(99), &Value::Null)
    );
}

/// The actor, action, outcome, summary and repeats of each row of a
/// ledger page.
fn brief(page: &Value) -> Vec<Value> {
    page["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            json!([
                r["actor"],
                r["action"],
                r["outcome"],
                r["summary"],
                r["repeats"]
            ])
        })
        .collect()
}

/// A destructive call whose job the queue refused leaves the call to add
/// its refused row; a queued one hands the row to the writer.
#[tokio::test]
async fn a_destructive_job_the_queue_refused_leaves_the_row_to_the_call() {
    use super::Destructive;
    use crate::{
        access::{Actor, Caller, Grant},
        ledger::{Entry, What, Who},
        store::{Job, Write},
    };
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let caller = Caller {
        actor: Actor::Token {
            id: 1,
            name: "ci".into(),
        },
        grant: Grant {
            level: Level::Full,
            destructive: true,
        },
    };
    let recorded = Arc::new(AtomicBool::new(false));
    let d = Destructive {
        unlocked: caller.unlocked().unwrap(),
        entry: Entry::of(Who::Token("ci".into()), "set_retention", What::default()).destructive(),
        recorded: Arc::clone(&recorded),
    };
    let cut = || {
        let change = retention::Change {
            project: "api".into(),
            logs_days: Some(retention::Setting::Days(7)),
            errors_days: None,
        };
        Job::SetRetention(change, retention::By::Agent(Defaults::default()))
    };
    let (writes, jobs) = crossbeam_channel::bounded(1);
    let (done, _) = tokio::sync::oneshot::channel();
    writes.try_send(Write { job: cut(), done }).unwrap();
    let busy = d.submit(&writes, cut()).await;
    assert!(matches!(busy, Err(store::Refused::Full)), "{busy:?}");
    assert!(!recorded.load(Ordering::Relaxed));
    drop(jobs.recv().unwrap());
    let call = tokio::spawn({
        let (d, writes) = (d.clone(), writes.clone());
        async move { d.submit(&writes, cut()).await }
    });
    let taken = tokio::task::spawn_blocking(move || jobs.recv().unwrap())
        .await
        .unwrap();
    assert!(recorded.load(Ordering::Relaxed));
    assert!(matches!(&taken.job, Job::Recorded { entry, .. } if entry.destructive));
    taken.done.send(Ok(())).unwrap();
    assert!(call.await.unwrap().is_ok());
}
