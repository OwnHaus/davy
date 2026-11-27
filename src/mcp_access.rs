//! Who may call what on `/mcp`. [`require`] admits the admin token or a
//! live agent token and puts the [`Caller`] in the request; the server
//! handler below lists only the tools its grant reaches and refuses a call
//! above it. Both ask [`access::need`], the one table of what each tool
//! needs. See "Agent tokens" in docs/design.md.

use super::{INSTRUCTIONS, Tools};
use crate::{
    access::{self, Actor, Caller, DESTRUCTIVE, Need},
    query,
    retention::{self, Change},
    store::{self, Job, Write},
    time,
    token::{self, Token},
    tokens,
};
use axum::{
    extract::{Request, State},
    http::{StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use rmcp::{
    ErrorData, RoleServer, ServerHandler,
    handler::server::tool::ToolCallContext,
    model::{
        CacheScope, CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock,
        ListToolsResult, PaginatedRequestParams, ProtocolVersion, ResultType, ServerCapabilities,
        ServerConfig, Tool,
    },
    service::RequestContext,
};
use rusqlite::Connection;
use std::sync::{Arc, Mutex};

/// What [`require`] checks a bearer token against.
pub(super) struct Gate {
    /// The admin token, when one is configured.
    pub(super) admin: Option<Token>,
    /// Reads `agent_tokens`, which [`query::open`] hides.
    pub(super) db: Mutex<Connection>,
    pub(super) writes: store::Sender,
}

/// Middleware that answers `401` unless the request carries the admin
/// token or a live agent token, and puts the [`Caller`] in the request's
/// extensions for the tools. The answer never says which of the two a
/// wrong token missed.
pub(super) async fn require(
    State(gate): State<Arc<Gate>>,
    mut req: Request,
    next: Next,
) -> Response {
    let presented = token::bearer(req.headers()).map(str::to_owned);
    let caller = match presented {
        Some(presented) => Arc::clone(&gate).caller(presented).await,
        None => None,
    };
    match caller {
        Some(caller) => {
            req.extensions_mut().insert(caller);
            next.run(req).await
        }
        None => (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
            "missing or wrong access token",
        )
            .into_response(),
    }
}

impl Gate {
    /// The caller `presented` names, if any. A failed read admits no one.
    /// The read of an agent token runs off the runtime, as it can wait
    /// for SQLite.
    async fn caller(self: Arc<Self>, presented: String) -> Option<Caller> {
        if self.admin.as_ref().is_some_and(|t| t.admits(&presented)) {
            return Some(Caller::admin());
        }
        if !presented.starts_with(tokens::PREFIX) {
            return None;
        }
        let now = time::now_ms();
        let gate = Arc::clone(&self);
        let found = tokio::task::spawn_blocking(move || {
            let db = gate.db.lock().ok()?;
            tokens::find(&db, &tokens::hash(&presented), now).ok()?
        })
        .await
        .ok()??;
        if found.stale {
            let (done, _) = tokio::sync::oneshot::channel();
            let job = Job::Token(tokens::Change::Used {
                id: found.id,
                at: now,
            });
            // A full queue skips this touch, and the next call tries again.
            drop(self.writes.try_send(Write { job, done }));
        }
        Some(Caller {
            actor: Actor::Token {
                id: found.id,
                name: found.name,
            },
            grant: found.grant,
        })
    }
}

/// The caller [`require`] put in the request, or `None` outside one.
fn caller(context: &RequestContext<RoleServer>) -> Option<Caller> {
    let parts = context.extensions.get::<axum::http::request::Parts>()?;
    parts.extensions.get::<Caller>().cloned()
}

fn refused(text: String) -> CallToolResponse {
    CallToolResponse::Complete(CallToolResult::error(vec![ContentBlock::text(text)]))
}

impl Tools {
    /// What this call of `name` needs: the table's need, or for a
    /// `set_retention` that would delete data or lose a person's floor,
    /// [`DESTRUCTIVE`] with why. Arguments the tool would refuse keep the
    /// table's need, and the tool says what is wrong.
    async fn need_of(
        &self,
        name: &str,
        request: &CallToolRequestParams,
    ) -> Option<(Need, Option<String>)> {
        let need = access::need(name)?;
        if name != "set_retention" {
            return Some((need, None));
        }
        let args = serde_json::Value::Object(request.arguments.clone().unwrap_or_default());
        let Ok(change) = serde_json::from_value::<Change>(args) else {
            return Some((need, None));
        };
        let defaults = self.retention;
        let deletes = self
            .blocking_with(move |data| {
                retention::destructive(&query::open(data)?, &change, defaults)
            })
            .await;
        // A failed read keeps the table's need: the writer refuses a cut
        // that carries no unlock all the same.
        Some(match deletes {
            Ok(Some(why)) => (DESTRUCTIVE, Some(why)),
            _ => (need, None),
        })
    }
}

#[expect(
    clippy::unused_async_trait_impl,
    reason = "list_tools awaits nothing, and the trait gives it a future"
)]
impl ServerHandler for Tools {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(INSTRUCTIONS)
    }

    /// Only the tools the caller's grant reaches.
    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let grant = caller(&context).map(|c| c.grant);
        let tools: Vec<Tool> = self
            .tool_router
            .list_all()
            .into_iter()
            .filter(|t| {
                access::need(&t.name)
                    .zip(grant)
                    .is_some_and(|(n, g)| g.admits(n))
            })
            .collect();
        let cache_hints = context
            .protocol_version()
            .is_some_and(|v| v >= ProtocolVersion::V_2026_07_28);
        Ok(ListToolsResult {
            result_type: Some(ResultType::COMPLETE),
            tools,
            meta: None,
            next_cursor: None,
            ttl_ms: cache_hints.then_some(0),
            // The list depends on the token, so no cache may share it.
            cache_scope: cache_hints.then_some(CacheScope::Private),
        })
    }

    /// Every tool, for the transport's check of request headers against
    /// a tool's arguments. A call still passes [`ServerHandler::call_tool`].
    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.tool_router.get(name).cloned()
    }

    /// Runs the call when the caller's grant reaches its need, and hands a
    /// destructive one, and no other, the [`access::Unlocked`] proof. A tool the table
    /// does not name is not found, whether the router has it or not.
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        mut context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let name = request.name.to_string();
        let Some(caller) = caller(&context) else {
            return Err(ErrorData::internal_error("no caller on this request", None));
        };
        let Some((need, why)) = self.need_of(&name, &request).await else {
            return Err(ErrorData::invalid_params("tool not found", None));
        };
        if !caller.grant.admits(need) {
            let refusal = caller.refusal(&name, need);
            return Ok(refused(match why {
                Some(why) => format!("{why}. {refusal}"),
                None => refusal,
            }));
        }
        if need.destructive
            && let Some(unlocked) = caller.unlocked()
        {
            context.extensions.insert(unlocked);
        }
        let call = ToolCallContext::new(self, request, context);
        self.tool_router.call(call).await
    }
}

#[cfg(test)]
#[path = "mcp_access_tests.rs"]
mod tests;
