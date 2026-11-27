//! The drop rule tools: an agent adds and removes shadow rules and never
//! a live one.

use super::*;

/// Applies the next `n` drop jobs as the store writer would.
fn drop_writer(s: &Server, n: usize) -> std::thread::JoinHandle<()> {
    let (data, writes) = (s.data.clone(), s.writes.clone());
    std::thread::spawn(move || {
        let c = store::open(&data).unwrap();
        for _ in 0..n {
            let w = next_job(&writes);
            let Job::Drop(change) = w.job else {
                panic!("expected a drop job");
            };
            w.done
                .send(crate::drops::apply(&c, &change).unwrap())
                .unwrap();
        }
    })
}

fn rules(s: &Server) -> Vec<(i64, bool, String)> {
    let c = store::open(&s.data).unwrap();
    crate::drops::load(&c, None, time::now_ms())
        .unwrap()
        .into_iter()
        .map(|r| (r.id, r.live, r.by.as_str().to_owned()))
        .collect()
}

fn person_adds_live(s: &Server, message: &str) {
    let c = store::open(&s.data).unwrap();
    let spec = crate::drops::Spec::new(None, None, Some(message), None, None).unwrap();
    crate::drops::add(&c, 1, &spec, true, crate::drops::By::Person, time::now_ms())
        .unwrap()
        .unwrap();
}

async fn listed_tools(app: &Router) -> Vec<Value> {
    let list = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"});
    let (_, body) = post(app, "localhost", Some(SECRET), list).await;
    let mut reply: Value = serde_json::from_str(&body).unwrap();
    reply["result"]["tools"].as_array_mut().unwrap().clone()
}

#[tokio::test]
async fn no_tool_makes_a_drop_rule_live_or_turns_one_live() {
    let s = server();
    let tools = listed_tools(&s.app).await;
    let mut names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "connect_project",
            "count_logs",
            "drop_rule_remove",
            "drop_rule_shadow",
            "drop_rules",
            "issue_context",
            "log_patterns",
            "mark_fixed",
            "project_status",
            "release_health",
            "schema",
            "search_logs",
            "set_retention",
            "sql",
            "submit_feedback",
            "usage",
        ]
    );
    let shadow = tools
        .iter()
        .find(|t| t["name"] == "drop_rule_shadow")
        .unwrap();
    let mut fields: Vec<&str> = shadow["inputSchema"]["properties"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    fields.sort_unstable();
    assert_eq!(
        fields,
        ["keep", "level", "message", "pattern", "project", "service"]
    );
    assert_eq!(shadow["inputSchema"]["additionalProperties"], false);

    // A rule asked to be live is refused before it reaches the writer.
    let live = json!({"project": "api", "message": "poll", "live": true});
    let (text, is_error) = tool(&s.app, "drop_rule_shadow", live).await;
    assert!(is_error && text.contains("unknown field `live`"), "{text}");
    assert!(
        s.writes.try_iter().all(|w| !matches!(w.job, Job::Drop(_))),
        "no drop job was queued"
    );

    // A person's live rule stays live and stays: the agent's equal shadow
    // rule is refused, and so is removing it.
    person_adds_live(&s, "poll");
    let writer = drop_writer(&s, 2);
    let (text, is_error) = tool(
        &s.app,
        "drop_rule_shadow",
        json!({"project": "api", "message": "poll"}),
    )
    .await;
    assert_eq!(
        (text.as_str(), is_error),
        (
            "rule 1 matches the same way and is live: a person makes it a shadow rule with davy drop shadow 1",
            true
        )
    );
    let (text, is_error) = tool(
        &s.app,
        "drop_rule_remove",
        json!({"project": "api", "rule_id": 1}),
    )
    .await;
    assert_eq!(
        (text.as_str(), is_error),
        (
            "no shadow rule 1 of an agent in this project: a person removes the others with davy drop remove",
            true
        )
    );
    writer.join().unwrap();
    assert_eq!(rules(&s), [(1, true, "person".to_owned())]);
}

#[tokio::test]
async fn an_agents_shadow_rule_names_the_command_a_person_runs_to_make_it_live() {
    let s = server();
    let writer = drop_writer(&s, 3);
    let add = json!({"project": "api", "message": "GET /health*", "service": "web", "keep": "1%"});
    let (text, is_error) = tool(&s.app, "drop_rule_shadow", add.clone()).await;
    assert!(!is_error, "{text}");
    let mut answer: Value = serde_json::from_str(&text).unwrap();
    let rule = answer["rule"].as_object_mut().unwrap();
    let created = rule.remove("created_at").unwrap();
    let expires = rule.remove("expires_at").unwrap();
    let at = |v: &Value| v.as_str().unwrap().parse::<jiff::Timestamp>().unwrap();
    assert_eq!(
        at(&expires).duration_since(at(&created)).as_hours(),
        14 * 24
    );
    let data = crate::install::sh(&s.data.display().to_string());
    assert_eq!(
        answer,
        json!({
            "rule": {
                "id": 1,
                "project": "api",
                "state": "shadow",
                "by": "agent",
                "position": 1,
                "pattern": null,
                "any_level": false,
                "level": null,
                "message": "GET /health*",
                "service": "web",
                "keep_percent": 1,
                "last_7_days": {
                    "dropped": {"records": 0, "bytes": "0 B"},
                    "would_drop": {"records": 0, "bytes": "0 B"},
                },
                "make_live": format!("a person can run: davy drop live --data {data} 1"),
            },
            "note": drop_tools::NOTE,
        })
    );
    // The same rule again is the same rule.
    let (again, _) = tool(&s.app, "drop_rule_shadow", add).await;
    assert_eq!(
        serde_json::from_str::<Value>(&again).unwrap()["rule"]["id"],
        1
    );

    let (text, _) = tool(&s.app, "drop_rules", json!({"project": "api"})).await;
    let listed: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(listed["rules"].as_array().unwrap().len(), 1);
    assert_eq!(listed["rules"][0]["state"], "shadow");

    let (text, is_error) = tool(
        &s.app,
        "drop_rule_remove",
        json!({"project": "api", "rule_id": 1}),
    )
    .await;
    assert_eq!((text.as_str(), is_error), (r#"{"removed":1}"#, false));
    writer.join().unwrap();
    assert_eq!(rules(&s), []);
}

#[tokio::test]
async fn an_agent_stages_no_rule_on_davys_own_project_nor_on_warnings() {
    let s = server();
    let mut c = store::open(&s.data).unwrap();
    let (own, _) = crate::self_report::own::find_or_create(&mut c).unwrap();
    let writer = drop_writer(&s, 3);
    let (text, is_error) = tool(
        &s.app,
        "drop_rule_shadow",
        json!({"project": own.slug, "level": "debug"}),
    )
    .await;
    assert_eq!(
        (text.as_str(), is_error),
        (
            "project \"davy\" is Davy's own, whose warnings and errors davy doctor and project_status read: an agent stages no rule on it, a person can with davy drop add",
            true
        )
    );
    for level in ["warn", "fatal"] {
        let (text, is_error) = tool(
            &s.app,
            "drop_rule_shadow",
            json!({"project": "api", "level": level}),
        )
        .await;
        assert_eq!(
            (text.as_str(), is_error),
            (
                format!("a level rule of {level} names warnings and errors: an agent stages a level rule below warn only, a person can add this one with davy drop add").as_str(),
                true
            )
        );
    }
    writer.join().unwrap();
    assert_eq!(rules(&s), []);
}

#[test]
fn an_answer_escapes_what_could_reorder_or_control_a_terminal() {
    assert_eq!(
        json(&json!({"message": "a\u{202e}b\u{85}c\u{2066}d"})).unwrap(),
        r#"{"message":"a\u202eb\u0085c\u2066d"}"#
    );
}

#[test]
fn without_the_proof_drop_rule_live_makes_no_change() {
    let refused = drop_tools::live_change(None, "api".to_owned(), 1, time::now_ms());
    assert_eq!(
        refused.err().as_deref(),
        Some("drop_rule_live needs a token of level full with destructive changes unlocked")
    );
}
