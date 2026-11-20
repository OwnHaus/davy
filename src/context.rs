//! Everything an agent needs to start fixing one issue, in one response.

use crate::{
    event::{self, ErrorEvent},
    logs::store::{LogHit, LogQuery, search},
    store,
};
use anyhow::Result;
use rusqlite::Connection;
use serde::Serialize;
use serde_json::Value;
use std::path::Path;

const HOUR_NS: i64 = 3_600_000_000_000;
const SECOND_NS: i64 = 1_000_000_000;

#[derive(Debug, Serialize)]
pub struct IssueContext {
    pub issue: store::IssueDetail,
    /// `event::compact` of the newest stored event.
    pub latest_event: Option<Value>,
    pub recent_events: Vec<store::EventRow>,
    pub releases: Vec<store::ReleaseCount>,
    /// Which logs `logs` holds, in words.
    pub logs_scope: String,
    /// Oldest first.
    pub logs: Vec<LogHit>,
}

/// `None` when there is no such issue. Logs come from the latest event's trace
/// within an hour either side, or from the whole project in the minute before
/// the event when it has no trace.
pub fn issue_context(c: &Connection, data: &Path, id: i64) -> Result<Option<IssueContext>> {
    let Some(issue) = store::issue(c, id)? else {
        return Ok(None);
    };
    let latest = store::payload(c, id)?.map(ErrorEvent::parse).transpose()?;
    let at_ns = latest
        .as_ref()
        .map_or(issue.last_seen_ms, |e| e.timestamp)
        .saturating_mul(1_000_000);
    let (logs_scope, query) = match latest.as_ref().and_then(|e| e.trace_id.as_deref()) {
        Some(trace) => (
            format!("trace {trace}, one hour either side of the event"),
            LogQuery {
                project: Some(issue.project_id),
                since: at_ns.saturating_sub(HOUR_NS),
                until: at_ns.saturating_add(HOUR_NS),
                min_level: None,
                text: Some(format!("trace_id:{trace}")),
                limit: 50,
            },
        ),
        None => (
            "project, 60 seconds before to 10 seconds after the event".to_owned(),
            LogQuery {
                project: Some(issue.project_id),
                since: at_ns.saturating_sub(60 * SECOND_NS),
                until: at_ns.saturating_add(10 * SECOND_NS),
                min_level: None,
                text: None,
                limit: 30,
            },
        ),
    };
    let logs = search(&crate::logs::dir(data), &query)?;
    Ok(Some(IssueContext {
        latest_event: latest.as_ref().map(event::compact),
        recent_events: store::recent_events(c, id)?,
        releases: store::issue_releases(c, id)?,
        issue,
        logs_scope,
        logs,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn store_with(event: Value) -> (std::path::PathBuf, Connection) {
        let data = crate::test_support::scratch("context");
        let c = store::open(&data).unwrap();
        store::create_project(&c, "api").unwrap();
        store::insert(&c, 1, &ErrorEvent::parse(event).unwrap()).unwrap();
        (data, c)
    }

    fn fixture_event() -> Value {
        let body = crate::test_support::fixture("sentry-node-10/4.body");
        let envelope = crate::envelope::Envelope::parse(&body).unwrap();
        serde_json::from_slice(&envelope.items[0].payload).unwrap()
    }

    #[test]
    fn context_of_a_node_error_has_source_runtime_and_trace_scope() {
        let (data, c) = store_with(fixture_event());
        let ctx = issue_context(&c, &data, 1).unwrap().unwrap();
        let ctx = serde_json::to_value(ctx).unwrap();
        assert_eq!(ctx["issue"]["project"], "api");
        assert_eq!(ctx["issue"]["status"], "unresolved");
        let event = &ctx["latest_event"];
        assert_eq!(event["timestamp"], "2026-11-20T08:42:01.048Z");
        assert_eq!(event["release"], "e2e@1.0.0");
        assert_eq!(
            event["runtime"],
            json!({"name": "node", "version": "v24.22.2"})
        );
        assert_eq!(event["user"], json!({"id": "user-42"}));
        let frames = &event["exceptions"][0]["frames"];
        assert_eq!(frames[1]["function"], "Server.?");
        assert_eq!(frames[1]["context_line"], "    JSON.parse('{nope')");
        assert_eq!(
            frames[2],
            json!({"filename": "<anonymous>", "module": "<anonymous>", "function": "JSON.parse", "in_app": false})
        );
        assert_eq!(event["exceptions"][0]["handled"], true);
        assert_eq!(
            ctx["releases"],
            json!([{"release": "e2e@1.0.0", "events": 1, "last_seen": "2026-11-20T08:42:01.048Z"}])
        );
        assert_eq!(
            ctx["recent_events"][0]["trace_id"],
            "f9e1079522134d8194d62046d471f90c"
        );
        assert_eq!(
            ctx["logs_scope"],
            "trace f9e1079522134d8194d62046d471f90c, one hour either side of the event"
        );
        assert_eq!(ctx["logs"], json!([]));
    }

    #[test]
    fn context_keeps_the_innermost_frames_and_the_last_breadcrumbs() {
        let frames: Vec<Value> = (0..40)
            .map(
                |i| json!({"function": format!("f{i}"), "in_app": false, "context_line": "hidden"}),
            )
            .collect();
        let breadcrumbs: Vec<Value> = (0..25)
            .map(|i| json!({"message": format!("b{i}"), "timestamp": 1_795_164_121}))
            .collect();
        let (data, c) = store_with(json!({
            "exception": {"values": [{"type": "Error", "stacktrace": {"frames": frames}}]},
            "breadcrumbs": {"values": breadcrumbs},
        }));
        let ctx = serde_json::to_value(issue_context(&c, &data, 1).unwrap().unwrap()).unwrap();
        let event = &ctx["latest_event"];
        let frames = event["exceptions"][0]["frames"].as_array().unwrap();
        assert_eq!(frames.len(), 30);
        assert_eq!(frames[0], json!({"function": "f10", "in_app": false}));
        let crumbs = event["breadcrumbs"].as_array().unwrap();
        assert_eq!(crumbs.len(), 20);
        assert_eq!(
            crumbs[0],
            json!({"message": "b5", "timestamp": "2026-11-20T08:42:01Z"})
        );
        assert_eq!(
            ctx["logs_scope"],
            "project, 60 seconds before to 10 seconds after the event"
        );
    }

    #[test]
    fn context_of_an_unknown_issue_is_none() {
        let (data, c) = store_with(json!({"message": "boom"}));
        assert!(issue_context(&c, &data, 99).unwrap().is_none());
    }
}
