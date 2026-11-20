//! The MCP server agents use to read and triage errors and logs. `http::serve`
//! mounts it at `/mcp` when an access token is configured.

use crate::{
    context,
    feedback::Feedback,
    logs::{
        self, LogLevel,
        store::{LogQuery, search},
    },
    query,
    store::{self, Job, Write},
    time,
    token::{self, Token},
};
use axum::{Router, middleware};
use rmcp::{
    RoleServer, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{ServerCapabilities, ServerConfig},
    schemars::JsonSchema,
    service::RequestContext,
    tool, tool_handler, tool_router,
    transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    },
};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
};

const MAX_LOGS: usize = 500;

const INSTRUCTIONS: &str = "Davy stores the errors that Sentry SDKs report and the logs that services ship. \
To triage, list open issues with `sql` (SELECT id,title,event_count,last_seen FROM issues WHERE status='unresolved' ORDER BY last_seen DESC LIMIT 20), \
then call `issue_context` on one issue. It returns the stack with source lines, breadcrumbs, releases and the logs around the error. \
Call `schema` before writing other SQL. After a fix ships, call `mark_fixed`. \
When a tool was missing, returned the wrong thing or needed a workaround, call `submit_feedback`.";

const LOG_NOTES: &str = "
Logs are not in SQL. Use search_logs. Its query is Tantivy syntax over these fields:
- message (full text, the default field)
- attributes.<key> (full text inside the JSON attributes, e.g. attributes.provider:acme)
- service, trace_id, span_id (exact match, e.g. trace_id:4bf92f3577b34da6a3ce929d0e0e4736)
Combine with AND, OR, NOT and parentheses. Levels are trace, debug, info, warn, error, fatal.";

/// `/mcp`, behind the token. Requests must name loopback or `public_host` in
/// their `Host` header, which stops DNS rebinding from a browser.
pub fn router(
    data: PathBuf,
    writes: mpsc::SyncSender<Write>,
    token: Token,
    public_host: &str,
) -> Router {
    let tools = Tools {
        data: Arc::from(data),
        writes,
        tool_router: Tools::tool_router(),
    };
    let config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .with_allowed_hosts(["localhost", "127.0.0.1", "::1", public_host]);
    let service: StreamableHttpService<Tools, LocalSessionManager> =
        StreamableHttpService::new(move || Ok(tools.clone()), Arc::default(), config);
    Router::new()
        .nest_service("/mcp", service)
        .layer(middleware::from_fn_with_state(
            Arc::new(token),
            token::require,
        ))
}

#[derive(Clone)]
struct Tools {
    data: Arc<Path>,
    writes: mpsc::SyncSender<Write>,
    tool_router: ToolRouter<Self>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct Sql {
    /// One SQLite statement that only reads, such as a SELECT or WITH.
    query: String,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct SearchLogs {
    /// Tantivy query, see `schema`. Leave out to match every log.
    query: Option<String>,
    /// Project slug. Leave out to search every project.
    project: Option<String>,
    /// RFC 3339 timestamp or a duration back from now: 30m, 1h, 2d. Default 24h.
    since: Option<String>,
    /// RFC 3339 timestamp or a duration back from now. Default now.
    until: Option<String>,
    /// Minimum level: trace, debug, info, warn, error or fatal.
    level: Option<String>,
    /// At most 500. Default 100. The newest logs win when there are more.
    limit: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct Issue {
    issue_id: i64,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct MarkFixed {
    issue_id: i64,
    /// The release that contains the fix. Leave out to mean any release first seen after now.
    release: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct SubmitFeedback {
    /// What you tried: the tool and its arguments, or the doc step.
    tried: String,
    /// What happened instead of what you needed.
    happened: String,
    /// How Davy could fix it, when you know.
    fix: Option<String>,
}

type Reply = Result<String, String>;

#[tool_router]
impl Tools {
    #[tool(
        description = "The SQL tables (projects, issues, events, releases, issue_hourly), what their columns mean, and how to search logs. Read it before writing SQL."
    )]
    async fn schema(&self) -> Reply {
        self.blocking(|data| Ok(query::schema(&query::open(data)?)? + LOG_NOTES))
            .await
    }

    #[tool(
        description = "Run one read-only SQLite query over issues and events. Returns columns, rows and whether rows were cut at 500. events.payload holds the full event; read it with json_extract(payload_json(payload), '$.path')."
    )]
    async fn sql(&self, Parameters(Sql { query }): Parameters<Sql>) -> Reply {
        self.blocking(move |data| json(&query::run(&query::open(data)?, &query)?))
            .await
    }

    #[tool(
        description = "Search logs from every source (Sentry, OpenTelemetry, Logtail shippers). Returns hits oldest first with level, service, message, trace_id and attributes."
    )]
    async fn search_logs(&self, Parameters(req): Parameters<SearchLogs>) -> Reply {
        self.blocking(move |data| {
            let now = time::now_ns();
            let instant = |s: Option<&str>, default: &str| {
                time::instant(s.unwrap_or(default), now).map_err(anyhow::Error::msg)
            };
            let project = match &req.project {
                Some(slug) => Some(store::project_by_slug(&query::open(data)?, slug)?),
                None => None,
            };
            let min_level = req
                .level
                .as_deref()
                .map(LogLevel::named)
                .transpose()
                .map_err(anyhow::Error::msg)?;
            let hits = search(
                &logs::dir(data),
                &LogQuery {
                    project,
                    since: instant(req.since.as_deref(), "24h")?,
                    until: instant(req.until.as_deref(), "0s")?,
                    min_level,
                    text: req.query,
                    limit: req.limit.unwrap_or(100).min(MAX_LOGS),
                },
            )?;
            json(&hits)
        })
        .await
    }

    #[tool(
        description = "Everything needed to start fixing one issue: the issue, its newest event cut to the useful parts (stack with source lines of in-app frames, breadcrumbs, tags, user, request, runtime), the last 10 events, the releases it appeared in, and the logs of the same trace or from just before the error."
    )]
    async fn issue_context(&self, Parameters(Issue { issue_id }): Parameters<Issue>) -> Reply {
        self.blocking(move |data| {
            let c = query::open(data)?;
            json(
                &context::issue_context(&c, data, issue_id)?
                    .ok_or_else(|| anyhow::anyhow!("no issue {issue_id}"))?,
            )
        })
        .await
    }

    #[tool(
        description = "Mark an issue resolved. Give the release that contains the fix when you know it. The issue reopens if the error comes back in that release or a newer one, but not from older releases still running."
    )]
    async fn mark_fixed(
        &self,
        Parameters(MarkFixed { issue_id, release }): Parameters<MarkFixed>,
    ) -> Reply {
        let reply = match &release {
            Some(r) => format!("issue {issue_id} resolved in release {r}"),
            None => format!("issue {issue_id} resolved until a new release reports it"),
        };
        let job = Job::Resolve {
            issue: issue_id,
            release,
            at: time::now_ms(),
        };
        store::submit(&self.writes, job)
            .await
            .map_err(|e| e.to_string())?;
        Ok(reply)
    }

    #[tool(
        description = "Report a problem with Davy itself: a tool that was missing, returned the wrong thing or needed a workaround, or a step in its docs that failed. Make one call per problem. Do not copy data from the errors or logs you read. The person who runs Davy reads it with `davy feedback`."
    )]
    async fn submit_feedback(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(SubmitFeedback {
            tried,
            happened,
            fix,
        }): Parameters<SubmitFeedback>,
    ) -> Reply {
        let feedback = Feedback::new(client_name(&ctx).as_deref(), tried, happened, fix)?;
        let job = Job::Feedback {
            at: time::now_ms(),
            feedback,
        };
        store::submit(&self.writes, job)
            .await
            .map_err(|e| e.to_string())?;
        Ok("Thanks. The feedback is stored for the person who runs Davy.".to_owned())
    }
}

/// The client's name from the request metadata, or its `User-Agent`. The
/// server keeps no sessions, so the name from `initialize` is gone and rmcp
/// fills in its own default.
fn client_name(ctx: &RequestContext<RoleServer>) -> Option<String> {
    let placeholder = rmcp::model::Implementation::default().name;
    ctx.client_info()
        .map(|info| info.name)
        .filter(|name| !name.is_empty() && *name != placeholder)
        .or_else(|| {
            let parts = ctx.extensions.get::<axum::http::request::Parts>()?;
            let agent = parts.headers.get(axum::http::header::USER_AGENT)?;
            agent.to_str().ok().map(str::to_owned)
        })
}

impl Tools {
    /// Runs blocking store or index work off the async runtime. Errors become
    /// tool errors the agent can read and act on.
    async fn blocking(
        &self,
        f: impl FnOnce(&Path) -> anyhow::Result<String> + Send + 'static,
    ) -> Reply {
        let data = Arc::clone(&self.data);
        tokio::task::spawn_blocking(move || f(&data))
            .await
            .map_err(|e| format!("tool crashed: {e}"))?
            .map_err(|e| format!("{e:#}"))
    }
}

fn json(v: &impl Serialize) -> anyhow::Result<String> {
    Ok(serde_json::to_string(v)?)
}

#[tool_handler(router = self.tool_router)]
#[expect(
    clippy::unused_async_trait_impl,
    reason = "the generated list_tools is async because the trait says so"
)]
impl ServerHandler for Tools {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(INSTRUCTIONS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::header;
    use serde_json::{Value, json};
    use tower::ServiceExt;

    const SECRET: &str = "a-test-token-that-is-long-enough!";

    struct Server {
        app: Router,
        writes: mpsc::Receiver<Write>,
    }

    fn server() -> Server {
        let data = crate::test_support::scratch("mcp");
        let c = store::open(&data).unwrap();
        store::create_project(&c, "api").unwrap();
        let (tx, writes) = mpsc::sync_channel(1);
        Server {
            app: router(data, tx, Token::new(SECRET), "davy.example"),
            writes,
        }
    }

    async fn post(app: &Router, host: &str, token: Option<&str>, body: Value) -> (u16, String) {
        let mut req = axum::http::Request::post("/mcp")
            .header(header::HOST, host)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json, text/event-stream");
        if let Some(t) = token {
            req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
        }
        let r = app
            .clone()
            .oneshot(req.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = r.status().as_u16();
        let body = to_bytes(r.into_body(), 1_000_000).await.unwrap();
        (status, String::from_utf8(body.to_vec()).unwrap())
    }

    fn call(tool: &str, arguments: &Value) -> Value {
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
               "params": {"name": tool, "arguments": arguments}})
    }

    /// The text and `isError` flag of a tool result.
    async fn tool(app: &Router, tool: &str, arguments: Value) -> (String, bool) {
        let (status, body) = post(app, "localhost", Some(SECRET), call(tool, &arguments)).await;
        assert_eq!(status, 200, "{body}");
        let reply: Value = serde_json::from_str(&body).unwrap();
        let result = &reply["result"];
        (
            result["content"][0]["text"].as_str().unwrap().to_owned(),
            result["isError"].as_bool().unwrap_or(false),
        )
    }

    #[tokio::test]
    async fn mcp_answers_only_with_the_token() {
        let s = server();
        let list = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"});
        for (token, expected) in [(None, 401), (Some("wrong"), 401), (Some(SECRET), 200)] {
            let (status, body) = post(&s.app, "localhost", token, list.clone()).await;
            assert_eq!(status, expected, "{token:?}: {body}");
        }
    }

    #[tokio::test]
    async fn mcp_answers_only_for_loopback_and_the_public_host() {
        let s = server();
        let list = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"});
        for (host, expected) in [
            ("localhost:8080", 200),
            ("127.0.0.1", 200),
            ("davy.example", 200),
            ("attacker.example", 403),
        ] {
            let (status, body) = post(&s.app, host, Some(SECRET), list.clone()).await;
            assert_eq!(status, expected, "{host}: {body}");
        }
    }

    #[tokio::test]
    async fn sql_tool_reads_and_refuses_writes() {
        let s = server();
        assert_eq!(
            tool(&s.app, "sql", json!({"query": "SELECT slug FROM projects"})).await,
            (
                r#"{"columns":["slug"],"rows":[["api"]],"truncated":false}"#.to_owned(),
                false
            )
        );
        let (text, is_error) = tool(&s.app, "sql", json!({"query": "DELETE FROM projects"})).await;
        assert!(is_error, "{text}");
    }

    #[tokio::test]
    async fn mark_fixed_queues_a_resolve_and_reports_the_writer_outcome() {
        let s = server();
        let writer = std::thread::spawn(move || {
            for outcome in [Ok(()), Err("no issue 9".to_owned())] {
                let w = s.writes.recv().unwrap();
                let Job::Resolve { issue, release, .. } = w.job else {
                    panic!("expected a resolve job");
                };
                w.done.send(outcome).unwrap();
                assert_eq!(release.as_deref(), Some("2.0.0"));
                assert!(issue == 7 || issue == 9);
            }
        });
        assert_eq!(
            tool(
                &s.app,
                "mark_fixed",
                json!({"issue_id": 7, "release": "2.0.0"})
            )
            .await,
            ("issue 7 resolved in release 2.0.0".to_owned(), false)
        );
        assert_eq!(
            tool(
                &s.app,
                "mark_fixed",
                json!({"issue_id": 9, "release": "2.0.0"})
            )
            .await,
            ("no issue 9".to_owned(), true)
        );
        writer.join().unwrap();
    }

    #[tokio::test]
    async fn submit_feedback_queues_the_text_with_the_client_name() {
        let s = server();
        let writer = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for _ in 0..2 {
                let w = s.writes.recv().unwrap();
                let Job::Feedback { feedback, .. } = w.job else {
                    panic!("expected a feedback job");
                };
                seen.push(feedback);
                w.done.send(Ok(())).unwrap();
            }
            seen
        });
        let thanks = (
            "Thanks. The feedback is stored for the person who runs Davy.".to_owned(),
            false,
        );
        let mut with_meta = call(
            "submit_feedback",
            &json!({"tried": "sql on logs", "happened": "no such table: logs", "fix": "say logs are not in SQL"}),
        );
        with_meta["params"]["_meta"] = json!({
            "io.modelcontextprotocol/clientInfo": {"name": "claude-code", "version": "2.1.0"}
        });
        let (status, body) = post(&s.app, "localhost", Some(SECRET), with_meta).await;
        assert_eq!(status, 200, "{body}");
        let reply: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(reply["result"]["content"][0]["text"], thanks.0.as_str());
        let request = axum::http::Request::post("/mcp")
            .header(header::HOST, "localhost")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json, text/event-stream")
            .header(header::AUTHORIZATION, format!("Bearer {SECRET}"))
            .header(header::USER_AGENT, "codex_cli_rs/0.80.0")
            .body(Body::from(
                call(
                    "submit_feedback",
                    &json!({"tried": "issue_context 3", "happened": "no source lines"}),
                )
                .to_string(),
            ))
            .unwrap();
        let r = s.app.clone().oneshot(request).await.unwrap();
        assert_eq!(r.status(), 200);
        assert_eq!(
            tool(
                &s.app,
                "submit_feedback",
                json!({"tried": "", "happened": "x"})
            )
            .await,
            ("tried is empty".to_owned(), true)
        );
        assert_eq!(
            writer.join().unwrap(),
            [
                Feedback {
                    client: Some("claude-code".to_owned()),
                    tried: "sql on logs".to_owned(),
                    happened: "no such table: logs".to_owned(),
                    fix: Some("say logs are not in SQL".to_owned()),
                },
                Feedback {
                    client: Some("codex_cli_rs/0.80.0".to_owned()),
                    tried: "issue_context 3".to_owned(),
                    happened: "no source lines".to_owned(),
                    fix: None,
                },
            ]
        );
    }
}
