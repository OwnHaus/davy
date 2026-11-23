//! The MCP server agents use to read and triage errors and logs. `http::serve`
//! mounts it at `/mcp` when an access token is configured.

use crate::{
    connect::{self, Sdk},
    context,
    feedback::Feedback,
    firsts::First,
    issues::IssueIds,
    logs::{
        self, LogLevel,
        store::{LogCounts, LogQuery, histogram, search},
    },
    query,
    store::{self, IssueStatus, Job},
    time,
    token::{self, Token},
};
use axum::{
    Router,
    extract::{Request, State},
    middleware::{self, Next},
    response::Response,
};
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
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

const MAX_LOGS: usize = 500;

const INSTRUCTIONS: &str = "Davy stores the errors that Sentry SDKs report and the logs that services ship. \
To connect an existing app, call `connect_project`, then send its test_event command and poll `project_status`. Create new projects locally with `davy connect SLUG --data DATA`; MCP cannot create ingest keys. \
To triage, list open issues with `sql` (SELECT id,title,event_count,last_seen FROM issues WHERE status='unresolved' ORDER BY last_seen DESC LIMIT 20), \
then call `issue_context` on one issue. It returns the stack with source lines, breadcrumbs, releases, the logs around the error and what users wrote about it. \
Call `schema` before writing other SQL. To see how often a log line happens and since when, call `count_logs` before `search_logs`. To see whether a release crashes more than the one before, call `release_health`. After a fix ships, call `mark_fixed`. \
When a tool was missing, returned the wrong thing or needed a workaround, call `submit_feedback`.";

const LOG_NOTES: &str = "
Logs are not in SQL. Use search_logs. Its query is Tantivy syntax over these fields:
- message and every attribute value (full text, the default: a bare word such as timeout or r-3fa9c2d1 matches either)
- attributes.<key> (full text in one attribute, e.g. attributes.provider:acme or attributes.req.id:r-3fa9c2d1)
- service, trace_id, span_id (exact match, e.g. trace_id:4bf92f3577b34da6a3ce929d0e0e4736)
Combine with AND, OR, NOT and parentheses. Levels are trace, debug, info, warn, error, fatal.
count_logs takes the same filters and returns counts per time bucket instead of the logs.";

/// `/mcp`, behind the token. Requests must name loopback or the host of
/// `public_url` in their `Host` header, and a browser page that sends one
/// must come from loopback or `public_url`, which stops DNS rebinding.
pub fn router(data: PathBuf, writes: store::Sender, token: Token, public_url: &url::Url) -> Router {
    let public_host = public_url.host_str().unwrap_or("localhost");
    let public_origin = format!(
        "{}://{public_host}:{}",
        public_url.scheme(),
        public_url.port_or_known_default().unwrap_or(443)
    );
    let tools = Tools {
        data: Arc::from(data),
        public_url: public_url.clone(),
        writes,
        tool_router: Tools::tool_router(),
    };
    let config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .with_allowed_hosts(["localhost", "127.0.0.1", "::1", public_host])
        .with_allowed_origins([
            public_origin.as_str(),
            "http://localhost:*",
            "http://127.0.0.1:*",
            "http://[::1]:*",
        ]);
    let seen = Arc::new(Seen {
        writes: tools.writes.clone(),
        once: AtomicBool::new(false),
    });
    let service: StreamableHttpService<Tools, LocalSessionManager> =
        StreamableHttpService::new(move || Ok(tools.clone()), Arc::default(), config);
    Router::new()
        .nest_service("/mcp", service)
        .layer(middleware::from_fn_with_state(seen, first_agent))
        .layer(middleware::from_fn_with_state(
            Arc::new(token),
            token::require,
        ))
}

/// Whether this server has recorded an agent on `/mcp` yet, for the setup
/// page.
struct Seen {
    writes: store::Sender,
    once: AtomicBool,
}

/// Records the first request that passed the token, once per server start.
/// The table keeps the first time of all.
async fn first_agent(State(seen): State<Arc<Seen>>, req: Request, next: Next) -> Response {
    if !seen.once.swap(true, Ordering::Relaxed) {
        let job = Job::First {
            what: First::Agent,
            at: time::now_ms(),
        };
        let writes = seen.writes.clone();
        tokio::spawn(async move { store::queue_and_wait(&writes, job).await });
    }
    next.run(req).await
}

#[derive(Clone)]
struct Tools {
    public_url: url::Url,
    data: Arc<Path>,
    writes: store::Sender,
    tool_router: ToolRouter<Self>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct Sql {
    /// One SQLite statement that only reads, such as a SELECT or WITH.
    query: String,
}

/// The logs `search_logs` reads and `count_logs` counts.
#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct LogFilter {
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
}

impl LogFilter {
    /// The store query, with at most `limit` hits.
    fn query(self, data: &Path, limit: usize) -> anyhow::Result<LogQuery> {
        let now = time::now_ns();
        let instant = |s: Option<&str>, default: &str| {
            time::instant(s.unwrap_or(default), now).map_err(anyhow::Error::msg)
        };
        let project = match &self.project {
            Some(slug) => Some(store::project_by_slug(&query::open(data)?, slug)?),
            None => None,
        };
        let min_level = self
            .level
            .as_deref()
            .map(LogLevel::named)
            .transpose()
            .map_err(anyhow::Error::msg)?;
        Ok(LogQuery {
            project,
            since: instant(self.since.as_deref(), "24h")?,
            until: instant(self.until.as_deref(), "0s")?,
            min_level,
            text: self.query,
            limit,
            skip: 0,
        })
    }
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct SearchLogs {
    #[serde(flatten)]
    filter: LogFilter,
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

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct ReleaseHealth {
    /// Project slug. Leave out for every project.
    project: Option<String>,
    /// RFC 3339 timestamp or a duration back from now: 1h, 2d, 30d. Default 7d. Davy keeps 90 days.
    since: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct ConnectProject {
    slug: String,
    sdk: Option<Sdk>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct ProjectStatus {
    slug: String,
}

type Reply = Result<String, String>;

#[tool_router]
impl Tools {
    #[tool(
        description = "Read an existing project's DSN, SDK setup and test-event command. Does not create projects: run davy connect on the host first. SDK names: android, rust, go, node, python, dotnet, browser."
    )]
    async fn connect_project(&self, Parameters(req): Parameters<ConnectProject>) -> Reply {
        let base = self.public_url.clone();
        self.blocking(move |data| json(&connect::existing(data, &req.slug, &base, req.sdk)?))
            .await
    }

    #[tool(
        description = "Confirm ingest for a project slug: events counted, time of the last event, issue count and time of the last log line, RFC 3339. Times are null before the first record; poll after sending a test event."
    )]
    async fn project_status(&self, Parameters(req): Parameters<ProjectStatus>) -> Reply {
        self.blocking(move |data| json(&connect::status(data, &req.slug)?))
            .await
    }

    #[tool(
        description = "The SQL tables (projects, issues, events, releases, issue_hourly, release_sessions, client_reports, user_feedback), what their columns mean, and how to search logs. Read it before writing SQL."
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
            let limit = req.limit.unwrap_or(100).min(MAX_LOGS);
            json(&search(&logs::dir(data), &req.filter.query(data, limit)?)?)
        })
        .await
    }

    #[tool(
        description = "Count the logs that match, per time bucket, with the filters of search_logs. Returns the total, the bucket width and each bucket that has logs as [start, count], oldest first, at most 200 buckets. Use it to see how often something happens and since when, then read examples with search_logs."
    )]
    async fn count_logs(&self, Parameters(filter): Parameters<LogFilter>) -> Reply {
        self.blocking(move |data| {
            let q = filter.query(data, 0)?;
            json(&LogCounts::from(histogram(&logs::dir(data), &q)?))
        })
        .await
    }

    #[tool(
        description = "Crash-free sessions and users per release and environment, from the sessions Sentry SDKs send. Returns, newest release first, sessions, errored (with an error, crashed ones too), crashed, abnormal, crash_free_sessions and crash_free_users in percent, users and the first and last hour. Use it to tell whether a new release crashes more than the one before. Server SDKs count requests as sessions and name no users."
    )]
    async fn release_health(&self, Parameters(req): Parameters<ReleaseHealth>) -> Reply {
        self.blocking(move |data| {
            let c = query::open(data)?;
            let project = match &req.project {
                Some(slug) => Some(store::project_by_slug(&c, slug)?),
                None => None,
            };
            let since = time::instant(req.since.as_deref().unwrap_or("7d"), time::now_ns())
                .map_err(anyhow::Error::msg)?
                / 1_000_000;
            json(&crate::items::health::releases(&c, project, since)?)
        })
        .await
    }

    #[tool(
        description = "Everything needed to start fixing one issue: the issue, its newest event cut to the useful parts (stack with source lines of in-app frames, breadcrumbs, tags, user, request, runtime), the last 10 events, the releases it appeared in, the logs of the same trace or from just before the error, and the newest 10 things users wrote about it through the SDK's feedback."
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
        let job = Job::SetStatus {
            issues: IssueIds::one(issue_id),
            status: IssueStatus::Resolved {
                in_release: release,
                at: time::now_ms(),
            },
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
#[path = "mcp_tests.rs"]
mod tests;
