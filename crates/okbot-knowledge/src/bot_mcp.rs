//! The `/bot` MCP server: memory tools for the agent, scoped by the run token.

use std::sync::Arc;

use okbot_core::{MemoryNote, MemoryScope, MemoryStore};
use rmcp::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorData,
    Implementation, ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig,
    Tool,
};
use rmcp::service::{MaybeSendFuture, RequestContext, RoleServer};
use serde_json::{Map, Value, json};

use crate::Knowledge;
use crate::tokens::RunTokens;

const INSTRUCTIONS: &str = "Memory tools for this chat bot.\n\
- memory_save: only when the user asks you to remember something, or states a lasting fact or \
preference about themselves. scope `user` for facts about the person you are talking to, `room` \
for facts about this chat, `global` only for facts useful to everyone. Write one short fact per \
note, in the user's language. Never save passwords, payment data or other secrets.\n\
- memory_list: what you remember here (use it before saving to avoid duplicates).\n\
- memory_forget: when the user asks you to forget something; pass the note id from memory_list.";

/// The memory MCP server; one instance serves every run, the token tells who is asking.
#[derive(Clone)]
pub(crate) struct BotServer {
    pub(crate) knowledge: Arc<Knowledge>,
    pub(crate) tokens: Arc<RunTokens>,
}

fn tool(name: &str, description: &str, schema: Value) -> Tool {
    let Value::Object(schema) = schema else {
        unreachable!("tool schemas are JSON objects")
    };
    Tool::new(name.to_string(), description.to_string(), Arc::new(schema))
}

fn tools() -> Vec<Tool> {
    vec![
        tool(
            "memory_save",
            "Save one short note to remember later.",
            json!({
                "type": "object",
                "properties": {
                    "text": {"type": "string", "description": "The fact to remember, one or two sentences."},
                    "scope": {"type": "string", "enum": ["user", "room", "global"],
                              "description": "user: about this person; room: about this chat; global: for everyone."},
                    "title": {"type": "string", "description": "Optional short title."}
                },
                "required": ["text", "scope"]
            }),
        ),
        tool(
            "memory_list",
            "List the notes remembered for this person, this chat and everyone.",
            json!({"type": "object", "properties": {}}),
        ),
        tool(
            "memory_forget",
            "Forget a note by its id (from memory_list).",
            json!({
                "type": "object",
                "properties": {"id": {"type": "string"}},
                "required": ["id"]
            }),
        ),
    ]
}

fn scope(value: &str) -> Option<MemoryScope> {
    match value {
        "user" => Some(MemoryScope::User),
        "room" => Some(MemoryScope::Room),
        "global" => Some(MemoryScope::Global),
        _ => None,
    }
}

impl BotServer {
    async fn call(
        &self,
        name: &str,
        args: &Map<String, Value>,
        token: Option<&str>,
    ) -> Result<String, String> {
        let context = token
            .and_then(|t| self.tokens.context(t))
            .ok_or_else(|| "this run has no access to memory".to_string())?;
        let who = &context.who;
        let k = &self.knowledge;
        match name {
            "memory_save" => {
                let text = args.get("text").and_then(Value::as_str).unwrap_or_default();
                let scope = args
                    .get("scope")
                    .and_then(Value::as_str)
                    .and_then(scope)
                    .ok_or("scope must be user, room or global")?;
                let title = args
                    .get("title")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let note = MemoryNote {
                    scope,
                    title,
                    text: text.to_string(),
                };
                let entry = k
                    .save_with_policy(who, note, context.policy)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(format!("Saved as {} ({:?}).", entry.id, entry.scope))
            }
            "memory_list" => {
                let entries = k.list(who).await.map_err(|e| e.to_string())?;
                if entries.is_empty() {
                    return Ok("No notes yet.".into());
                }
                Ok(entries
                    .iter()
                    .map(|e| {
                        format!(
                            "- {} [{:?}, {}] {}: {}",
                            e.id, e.scope, e.updated, e.title, e.text
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"))
            }
            "memory_forget" => {
                let id = args.get("id").and_then(Value::as_str).unwrap_or_default();
                let entry = k.forget(who, id).await.map_err(|e| e.to_string())?;
                Ok(format!("Forgot {}.", entry.id))
            }
            other => Err(format!("unknown tool {other}")),
        }
    }
}

impl ServerHandler for BotServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                "okbot-memory",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(INSTRUCTIONS.to_string())
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, ErrorData>> + MaybeSendFuture + '_ {
        std::future::ready(Ok(ListToolsResult::with_all_items(tools())))
    }

    fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResponse, ErrorData>> + MaybeSendFuture + '_ {
        let token = context
            .extensions
            .get::<axum::http::request::Parts>()
            .and_then(|p| p.headers.get(axum::http::header::AUTHORIZATION).cloned())
            .and_then(|v| v.to_str().ok().map(str::to_string))
            .and_then(|v| v.strip_prefix("Bearer ").map(|t| t.trim().to_string()));
        async move {
            let args = request.arguments.unwrap_or_default();
            let result = match self.call(&request.name, &args, token.as_deref()).await {
                Ok(text) => CallToolResult::success(vec![ContentBlock::text(text)]),
                Err(message) => CallToolResult::error(vec![ContentBlock::text(message)]),
            };
            Ok(result.into())
        }
    }
}

/// The streamable-HTTP router for the server (mounted at `/bot`).
pub(crate) fn router(server: BotServer) -> axum::Router {
    use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
    use rmcp::transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService,
    };
    let service = StreamableHttpService::new(
        move || Ok(server.clone()),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );
    axum::Router::new().nest_service("/mcp", service)
}
