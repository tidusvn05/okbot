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
- memory_forget: when the user asks you to forget something; pass the note id from memory_list.\n\
- memory_update: when a remembered fact changed; pass the note id and the new text.\n\
- note_add: quick notes and journal entries the user dictates (\"note: …\", \"ghi lại …\"); they are \
found later with the knowledge tools (folder notes/). Memory is for lasting facts, notes for a log.\n\
- knowledge_add: save a document, a page or an attached file to the knowledge base when the owner \
asks (\"lưu tài liệu này\").\n\
- task_add: to-dos and reminders the user asks for (\"remind me…\", \"add … to my list\"). Times are \
local (`YYYY-MM-DD HH:MM`, the bot's time zone, from the current time in your instructions). A \
reminder is sent by the bot at that time, even if nobody writes. Confirm the stored time in your answer.\n\
- task_list / task_update / task_done / task_delete: show and change tasks; snooze by updating \
remind_at. Use the id (`t-…`) from task_list or from a reminder message.\n\
- attachment_view: see an image the user sent with the current message (index from the message).";

/// Largest attachment fetched for the agent.
const MAX_ATTACHMENT_BYTES: usize = 8 << 20;

/// The memory MCP server; one instance serves every run, the token tells who is asking.
#[derive(Clone)]
pub(crate) struct BotServer {
    pub(crate) knowledge: Arc<Knowledge>,
    pub(crate) tokens: Arc<RunTokens>,
}

pub(crate) fn tool(name: &str, description: &str, schema: Value) -> Tool {
    let Value::Object(schema) = schema else {
        unreachable!("tool schemas are JSON objects")
    };
    Tool::new(name.to_string(), description.to_string(), Arc::new(schema))
}

fn tools() -> Vec<Tool> {
    let mut tools = vec![
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
            "attachment_view",
            "See an image the user attached to the current message.",
            json!({
                "type": "object",
                "properties": {"index": {"type": "integer", "minimum": 1,
                                         "description": "1 for the first attachment."}},
                "required": ["index"]
            }),
        ),
        tool(
            "memory_update",
            "Replace the text of a note (from memory_list) when the fact changed.",
            json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string"},
                    "text": {"type": "string", "description": "The new fact, one or two sentences."},
                    "title": {"type": "string"}
                },
                "required": ["id", "text"]
            }),
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
    ];
    tools.extend(crate::task_tools::tools());
    tools.extend(crate::inbox::tools());
    tools
}

pub(crate) fn str_arg<'a>(args: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

pub(crate) fn scope(value: &str) -> Option<MemoryScope> {
    match value {
        "user" => Some(MemoryScope::User),
        "room" => Some(MemoryScope::Room),
        "global" => Some(MemoryScope::Global),
        _ => None,
    }
}

/// A downloaded attachment.
pub(crate) struct Fetched {
    /// The content.
    pub(crate) bytes: Vec<u8>,
    /// Media type from the response (`image/png`), without parameters.
    pub(crate) mime: String,
}

/// Downloads `url` over HTTPS (redirects only to HTTPS, so internal hosts stay out of reach), up
/// to `max` bytes. `allow_http` is for tests against a local server.
pub(crate) async fn fetch(url: &str, allow_http: bool, max: usize) -> Result<Fetched, String> {
    if !(url.starts_with("https://") || allow_http && url.starts_with("http://")) {
        return Err("attachment URL is not HTTPS".into());
    }
    let policy = reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() >= 5 {
            attempt.error("too many redirects")
        } else if attempt.url().scheme() == "https" || allow_http {
            attempt.follow()
        } else {
            attempt.stop()
        }
    });
    let _ = rustls::crypto::ring::default_provider().install_default(); // see okbot-cli main
    let client = reqwest::Client::builder()
        .redirect(policy)
        .build()
        .map_err(|e| e.to_string())?;
    let response = client
        .get(url)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| format!("cannot download the file: {}", e.without_url()))?;
    if !response.status().is_success() {
        return Err(format!(
            "cannot download the file: HTTP {}",
            response.status()
        ));
    }
    let mime = response
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    if response
        .content_length()
        .is_some_and(|n| usize::try_from(n).map_or(true, |n| n > max))
    {
        return Err("the file is too large".into());
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|e| format!("cannot download the file: {}", e.without_url()))?;
    if bytes.len() > max {
        return Err("the file is too large".into());
    }
    Ok(Fetched {
        bytes: bytes.to_vec(),
        mime,
    })
}

/// Fetches attachment `index` (1-based) of the run: only URLs the platform delivered, over
/// HTTPS, images only, size-limited.
async fn fetch_image(
    attachments: &[okbot_core::Attachment],
    index: usize,
    allow_http: bool,
) -> Result<(String, String), String> {
    use base64::Engine as _;
    let attachment = index
        .checked_sub(1)
        .and_then(|i| attachments.get(i))
        .ok_or_else(|| format!("there is no attachment {index} in this message"))?;
    let okbot_core::Attachment::Image { url, .. } = attachment else {
        return Err(format!("attachment {index} is not an image"));
    };
    let fetched = fetch(url, allow_http, MAX_ATTACHMENT_BYTES)
        .await
        .map_err(|e| e.replace("the file", "the image"))?;
    if !fetched.mime.starts_with("image/") {
        return Err(format!(
            "attachment {index} is not an image ({})",
            fetched.mime
        ));
    }
    Ok((
        base64::engine::general_purpose::STANDARD.encode(&fetched.bytes),
        fetched.mime,
    ))
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
                let entries = MemoryStore::list(k.as_ref(), who)
                    .await
                    .map_err(|e| e.to_string())?;
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
            "memory_update" => {
                let id = str_arg(args, "id").unwrap_or_default();
                let text = str_arg(args, "text").unwrap_or_default().to_string();
                let title = str_arg(args, "title").map(str::to_string);
                let entry = k
                    .update_note(who, id, text, title)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(format!("Updated {}.", entry.id))
            }
            "note_add" => {
                let scope = match str_arg(args, "scope") {
                    None => MemoryScope::User,
                    Some(v) => scope(v)
                        .filter(|s| *s != MemoryScope::Global)
                        .ok_or("scope must be user or room")?,
                };
                let text = str_arg(args, "text").unwrap_or_default();
                let id = MemoryStore::note(k.as_ref(), who, scope, text)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(format!("Noted in {id}."))
            }
            "knowledge_add" => crate::inbox::knowledge_add(k, &context, args).await,
            "task_add" | "task_list" | "task_update" | "task_done" | "task_delete" => {
                crate::task_tools::call(k, who, context.admin, name, args).await
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
            if request.name == "attachment_view" {
                let index = args.get("index").and_then(Value::as_u64).unwrap_or(0);
                let attachments = token
                    .as_deref()
                    .and_then(|t| self.tokens.context(t))
                    .map(|c| c.attachments)
                    .unwrap_or_default();
                let index = usize::try_from(index).unwrap_or(0);
                let result = match fetch_image(&attachments, index, false).await {
                    Ok((data, mime)) => {
                        CallToolResult::success(vec![ContentBlock::image(data, mime)])
                    }
                    Err(message) => CallToolResult::error(vec![ContentBlock::text(message)]),
                };
                return Ok(result.into());
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use okbot_core::Attachment;

    async fn serve() -> String {
        let app = axum::Router::new()
            .route(
                "/cat.png",
                axum::routing::get(|| async {
                    (
                        [(axum::http::header::CONTENT_TYPE, "image/png")],
                        vec![137u8, 80, 78, 71],
                    )
                }),
            )
            .route(
                "/page",
                axum::routing::get(|| async {
                    ([(axum::http::header::CONTENT_TYPE, "text/html")], "<html>")
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        base
    }

    fn image(url: String) -> Attachment {
        Attachment::Image { url, caption: None }
    }

    #[tokio::test]
    async fn fetches_only_listed_https_images() {
        let base = serve().await;
        let list = vec![
            image(format!("{base}/cat.png")),
            image(format!("{base}/page")),
        ];
        let (data, mime) = fetch_image(&list, 1, true).await.unwrap();
        assert_eq!((data.as_str(), mime.as_str()), ("iVBORw==", "image/png"));
        assert!(
            fetch_image(&list, 2, true)
                .await
                .unwrap_err()
                .contains("not an image")
        );
        assert!(
            fetch_image(&list, 3, true)
                .await
                .unwrap_err()
                .contains("no attachment 3")
        );
        assert!(fetch_image(&list, 0, true).await.is_err());
        assert!(
            fetch_image(&list, 1, false)
                .await
                .unwrap_err()
                .contains("HTTPS"),
            "http refused in production"
        );
        let audio = vec![Attachment::Audio {
            url: format!("{base}/cat.png"),
        }];
        assert!(
            fetch_image(&audio, 1, true)
                .await
                .unwrap_err()
                .contains("not an image")
        );
    }
}
