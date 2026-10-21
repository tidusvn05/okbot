//! The MCP gateway (ADR 0011): the agent reaches `[[mcp]]` servers only through
//! `http://127.0.0.1:<port>/mcp/<name>`, with a token bound to its turn.
//!
//! For each request the gateway finds the turn, applies [`permission`](crate::permission)
//! (hiding tools in `tools/list`, refusing `tools/call`), asks for a person's approval when the
//! server's rules want it, forwards with the server's real credential (which the agent never
//! holds), and records each tool call. Replies are read until the JSON-RPC response and returned
//! as plain JSON.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use okbot_core::{CancellationToken, InboundEvent, RunId, Secret, ToolCallRecord, ToolDecision};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::Shared;
use crate::config::ExtraMcp;
use crate::mcp_client::{self, McpError, SESSION};
use crate::permission::{self, Who};
use crate::stdio::Process;

/// How long tool annotations from a listing are trusted.
const HINTS_TTL: Duration = Duration::from_secs(600);
/// Local server processes one turn may start.
const MAX_LOCAL_SESSIONS: usize = 8;

/// Request headers passed on to the server.
const FORWARDED: [&str; 3] = [SESSION, "mcp-protocol-version", "last-event-id"];

/// The turns that may use the gateway, and what it learned about tools.
#[derive(Default)]
pub(crate) struct Gateway {
    grants: Mutex<HashMap<String, Arc<Grant>>>,
    url: OnceLock<String>,
    /// Tool annotations by server and tool, from `tools/list` replies (`null` when a tool has
    /// none).
    annotations: Mutex<HashMap<(String, String), (Instant, Value)>>,
}

/// One turn's access: who asked, where, and the credential each server gets.
pub(crate) struct Grant {
    /// The message that started the turn.
    pub(crate) event: InboundEvent,
    /// The agent run.
    pub(crate) run_id: RunId,
    /// [`okbot_core::person_key`] of the requester.
    pub(crate) person: String,
    /// The requester's roles and room.
    pub(crate) who: Who,
    /// What each offered server receives as its bearer token.
    pub(crate) upstream: HashMap<String, Secret<String>>,
    /// The end of the turn's time.
    pub(crate) expires: Instant,
    /// Cancelled when the turn ends.
    pub(crate) done: CancellationToken,
    /// Local (stdio) server processes of this turn, by session id: `(server, process)`.
    pub(crate) local: Mutex<HashMap<String, (String, Arc<Process>)>>,
}

/// Ends a [`Grant`] when dropped: its token stops working and pending approvals close.
pub(crate) struct GrantGuard {
    gateway: Arc<Gateway>,
    key: String,
    done: CancellationToken,
}

impl Drop for GrantGuard {
    fn drop(&mut self) {
        self.done.cancel();
        let grant = lock(&self.gateway.grants).remove(&self.key);
        // Local servers stop with the turn, even while a request still holds the grant.
        if let Some(grant) = grant {
            lock(&grant.local).clear();
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn digest(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

impl Gateway {
    /// `http://127.0.0.1:<port>` once the gateway listens.
    pub(crate) fn url(&self) -> Option<&str> {
        self.url.get().map(String::as_str)
    }

    /// Registers a turn's grant; the token goes to the agent.
    pub(crate) fn issue(self: &Arc<Self>, grant: Grant) -> (Secret<String>, GrantGuard) {
        let token = hex::encode(rand::random::<[u8; 32]>());
        let key = digest(&token);
        let done = grant.done.clone();
        lock(&self.grants).insert(key.clone(), Arc::new(grant));
        (
            Secret::new(token),
            GrantGuard {
                gateway: self.clone(),
                key,
                done,
            },
        )
    }

    fn find(&self, token: &str) -> Option<Arc<Grant>> {
        let grant = lock(&self.grants).get(&digest(token)).cloned()?;
        (grant.expires > Instant::now() && !grant.done.is_cancelled()).then_some(grant)
    }

    fn remember(&self, server: &str, tools: &[Value]) {
        let mut known = lock(&self.annotations);
        for tool in tools {
            if let Some(name) = tool.get("name").and_then(Value::as_str) {
                known.insert(
                    (server.to_string(), name.to_string()),
                    (
                        Instant::now(),
                        tool.get("annotations").cloned().unwrap_or(Value::Null),
                    ),
                );
            }
        }
    }

    /// The annotations of a tool listed in the last [`HINTS_TTL`] (`null`: it has none); `None`
    /// when not listed lately, so a server that changes a tool is asked again.
    fn known(&self, server: &str, tool: &str) -> Option<Value> {
        lock(&self.annotations)
            .get(&(server.to_string(), tool.to_string()))
            .filter(|(at, _)| at.elapsed() < HINTS_TTL)
            .map(|(_, hints)| hints.clone())
    }
}

/// Listens on a random loopback port until `stop`.
pub(crate) async fn serve(
    shared: Arc<Shared>,
    stop: CancellationToken,
) -> std::io::Result<impl std::future::Future<Output = ()> + Send + 'static> {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
    let addr = listener.local_addr()?;
    let _ = shared.gateway.url.set(format!("http://{addr}"));
    tracing::info!(%addr, "MCP gateway listening");
    let app = router(shared);
    Ok(async move {
        if let Err(error) = axum::serve(listener, app)
            .with_graceful_shutdown(async move { stop.cancelled().await })
            .await
        {
            tracing::error!(%error, "MCP gateway failed");
        }
    })
}

pub(crate) fn router(shared: Arc<Shared>) -> Router {
    Router::new()
        .route(
            "/mcp/{name}",
            axum::routing::post(post)
                .delete(delete)
                // No server-initiated stream: allowed by the protocol, and nothing to relay.
                .get(|| async { StatusCode::METHOD_NOT_ALLOWED }),
        )
        .with_state(shared)
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
}

/// The grant and server a request may reach, or the refusal.
#[allow(clippy::result_large_err)] // a refusal is the response itself
fn admit(
    shared: &Shared,
    headers: &HeaderMap,
    name: &str,
) -> Result<(Arc<Grant>, usize, Secret<String>), Response> {
    let Some(grant) = bearer(headers).and_then(|t| shared.gateway.find(t)) else {
        return Err((StatusCode::UNAUTHORIZED, "a valid run token is required").into_response());
    };
    let found = shared
        .config
        .mcp
        .iter()
        .position(|m| m.server.name == name)
        .zip(grant.upstream.get(name).cloned());
    match found {
        Some((index, token)) => Ok((grant, index, token)),
        None => Err((StatusCode::NOT_FOUND, "no such server for this turn").into_response()),
    }
}

fn rpc_error(id: &Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn tool_error(id: &Value, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": {
        "content": [{"type": "text", "text": message}],
        "isError": true,
    }})
}

fn json_response(status: StatusCode, session: Option<&str>, body: &Value) -> Response {
    let mut response = (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response();
    if let Some(value) = session.and_then(|s| HeaderValue::from_str(s).ok()) {
        response.headers_mut().insert(SESSION, value);
    }
    response
}

async fn post(
    State(shared): State<Arc<Shared>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let (grant, index, token) = match admit(&shared, &headers, &name) {
        Ok(found) => found,
        Err(response) => return response,
    };
    let server = &shared.config.mcp[index];
    let Ok(message) = serde_json::from_slice::<Value>(&body) else {
        return json_response(
            StatusCode::BAD_REQUEST,
            None,
            &rpc_error(&Value::Null, -32700, "parse error"),
        );
    };
    if !message.is_object() {
        return json_response(
            StatusCode::BAD_REQUEST,
            None,
            &rpc_error(&Value::Null, -32600, "batches are not supported"),
        );
    }
    let id = message
        .get("id")
        .filter(|_| message.get("method").is_some());
    match (message.get("method").and_then(Value::as_str), id) {
        (Some("tools/call"), Some(id)) => {
            call(&shared, &grant, server, &token, &headers, &message, id).await
        }
        (Some("tools/list"), Some(id)) => {
            list(&shared, &grant, server, &token, &headers, &message, id).await
        }
        (Some(method), _) if relayed(method, id.is_some()) => {
            relay(&shared, &grant, server, &token, &headers, &message).await
        }
        // Resources, prompts and the rest would bypass the tool rules; a `tools/call` without an
        // id (a notification) would bypass them too.
        (Some(_), Some(id)) => json_response(
            StatusCode::OK,
            None,
            &rpc_error(id, -32601, "not offered through okbot"),
        ),
        _ => json_response(
            StatusCode::BAD_REQUEST,
            None,
            &rpc_error(&Value::Null, -32600, "not offered through okbot"),
        ),
    }
}

/// Methods passed on as they are: the session's own, and notifications other than tool calls.
fn relayed(method: &str, request: bool) -> bool {
    if request {
        matches!(method, "initialize" | "ping")
    } else {
        method.starts_with("notifications/")
    }
}

async fn delete(
    State(shared): State<Arc<Shared>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Response {
    let (grant, index, token) = match admit(&shared, &headers, &name) {
        Ok(found) => found,
        Err(response) => return response,
    };
    let server = &shared.config.mcp[index];
    if server.stdio.is_some() {
        let session = headers.get(SESSION).and_then(|v| v.to_str().ok());
        let ended = session.and_then(|id| lock(&grant.local).remove(id));
        return if ended.is_some() {
            StatusCode::OK
        } else {
            StatusCode::NOT_FOUND
        }
        .into_response();
    }
    let mut request = http_client()
        .delete(&server.server.url)
        .bearer_auth(token.expose())
        .timeout(Duration::from_secs(10));
    for name in FORWARDED {
        if let Some(value) = headers.get(name) {
            request = request.header(name, value.as_bytes());
        }
    }
    match request.send().await {
        Ok(reply) => StatusCode::from_u16(reply.status().as_u16())
            .unwrap_or(StatusCode::BAD_GATEWAY)
            .into_response(),
        Err(_) => StatusCode::BAD_GATEWAY.into_response(),
    }
}

fn http_client() -> reqwest::Client {
    let _ = rustls::crypto::ring::default_provider().install_default(); // see crates/okbot-cli/src/main.rs
    reqwest::Client::new()
}

/// The server's answer to one message.
struct Exchange {
    status: u16,
    session: Option<String>,
    /// The JSON-RPC response to a request, when found.
    response: Option<Value>,
    /// The body of a failed (non-2xx) exchange.
    body: Vec<u8>,
}

/// Sends one message to the server with its real credential.
async fn exchange(
    shared: &Shared,
    grant: &Grant,
    server: &ExtraMcp,
    token: &Secret<String>,
    headers: &HeaderMap,
    message: &Value,
) -> Result<Exchange, McpError> {
    if let Some(local) = &server.stdio {
        return exchange_local(shared, grant, server, local, headers, message).await;
    }
    let mut request = http_client()
        .post(&server.server.url)
        .bearer_auth(token.expose())
        .header("accept", "application/json, text/event-stream")
        .timeout(shared.config.agent_timeout + Duration::from_secs(30))
        .json(message);
    for name in FORWARDED {
        if let Some(value) = headers.get(name) {
            request = request.header(name, value.as_bytes());
        }
    }
    let mut reply = request.send().await.map_err(mcp_client::http_error)?;
    let status = reply.status().as_u16();
    let session = reply
        .headers()
        .get(SESSION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let sse = reply
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/event-stream"));
    let id = message
        .get("id")
        .filter(|_| message.get("method").is_some());
    let body = mcp_client::read_reply(&mut reply, sse, id).await?;
    let response = match id {
        Some(id) if reply_ok(status) => {
            mcp_client::find_response(&String::from_utf8_lossy(&body), id)
        }
        _ => None,
    };
    Ok(Exchange {
        status,
        session,
        response,
        body,
    })
}

/// Sends one message to a local server: `initialize` without a session starts a process.
async fn exchange_local(
    shared: &Shared,
    grant: &Grant,
    server: &ExtraMcp,
    local: &crate::config::StdioServer,
    headers: &HeaderMap,
    message: &Value,
) -> Result<Exchange, McpError> {
    let not_found = || Exchange {
        status: 404,
        session: None,
        response: None,
        body: b"no such session".to_vec(),
    };
    let session = headers
        .get(SESSION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let (id, process) = match session {
        Some(id) => match lock(&grant.local).get(&id) {
            Some((name, process)) if *name == server.server.name => (id, process.clone()),
            _ => return Ok(not_found()),
        },
        None if message.get("method").and_then(Value::as_str) == Some("initialize") => {
            if lock(&grant.local).len() >= MAX_LOCAL_SESSIONS {
                return Err(McpError::Protocol("too many local server sessions".into()));
            }
            let parent = shared.config.work_dir.join("mcp");
            let process = Process::spawn(local, &parent).map_err(|e| {
                tracing::warn!(mcp = %server.server.name, error = %e, "cannot start the local MCP server");
                McpError::Protocol("the local server cannot start".into())
            })?;
            let id = hex::encode(rand::random::<[u8; 16]>());
            let process = Arc::new(process);
            lock(&grant.local).insert(id.clone(), (server.server.name.clone(), process.clone()));
            tracing::info!(mcp = %server.server.name, "local MCP server started");
            (id, process)
        }
        None => {
            return Ok(Exchange {
                status: 400,
                session: None,
                response: None,
                body: b"initialize first".to_vec(),
            });
        }
    };
    let response = process
        .exchange(message, shared.config.agent_timeout)
        .await?;
    Ok(Exchange {
        status: if response.is_some() { 200 } else { 202 },
        session: Some(id),
        response,
        body: Vec::new(),
    })
}

fn reply_ok(status: u16) -> bool {
    (200..300).contains(&status)
}

/// What the agent gets for an exchange whose response is not changed.
fn pass(id: Option<&Value>, result: Result<Exchange, McpError>) -> Response {
    match result {
        Ok(ex) if !reply_ok(ex.status) => {
            let status = StatusCode::from_u16(ex.status).unwrap_or(StatusCode::BAD_GATEWAY);
            (status, ex.body).into_response()
        }
        Ok(Exchange {
            session,
            response: Some(response),
            ..
        }) => json_response(StatusCode::OK, session.as_deref(), &response),
        Ok(ex) => match id {
            Some(id) => json_response(
                StatusCode::BAD_GATEWAY,
                ex.session.as_deref(),
                &rpc_error(id, -32603, "the server sent no response"),
            ),
            None => {
                let mut response = StatusCode::from_u16(ex.status)
                    .unwrap_or(StatusCode::ACCEPTED)
                    .into_response();
                if let Some(value) = ex.session.and_then(|s| HeaderValue::from_str(&s).ok()) {
                    response.headers_mut().insert(SESSION, value);
                }
                response
            }
        },
        Err(error) => {
            tracing::warn!(%error, "MCP server unreachable through the gateway");
            let body = rpc_error(id.unwrap_or(&Value::Null), -32603, &error.to_string());
            json_response(StatusCode::BAD_GATEWAY, None, &body)
        }
    }
}

async fn relay(
    shared: &Shared,
    grant: &Grant,
    server: &ExtraMcp,
    token: &Secret<String>,
    headers: &HeaderMap,
    message: &Value,
) -> Response {
    let id = message
        .get("id")
        .filter(|_| message.get("method").is_some());
    pass(
        id,
        exchange(shared, grant, server, token, headers, message).await,
    )
}

/// `tools/list`: only the tools the requester may call here.
async fn list(
    shared: &Shared,
    grant: &Grant,
    server: &ExtraMcp,
    token: &Secret<String>,
    headers: &HeaderMap,
    message: &Value,
    id: &Value,
) -> Response {
    let mut result = exchange(shared, grant, server, token, headers, message).await;
    if let Ok(Exchange {
        response: Some(response),
        ..
    }) = &mut result
        && let Some(tools) = response
            .pointer_mut("/result/tools")
            .and_then(Value::as_array_mut)
    {
        shared.gateway.remember(&server.server.name, tools);
        tools.retain(|t| {
            t.get("name")
                .and_then(Value::as_str)
                .is_some_and(|name| permission::may_call(server, &grant.who, name))
        });
    }
    pass(Some(id), result)
}

/// The annotations of `tool`: remembered from a listing, else asked for. `None` (or `null`)
/// counts as the protocol's defaults.
async fn annotations(
    shared: &Shared,
    server: &ExtraMcp,
    token: &Secret<String>,
    tool: &str,
) -> Option<Value> {
    if let Some(known) = shared.gateway.known(&server.server.name, tool) {
        return Some(known);
    }
    if server.stdio.is_some() {
        // A local server is listed through the agent's own session only.
        return None;
    }
    let listed = mcp_client::with_session(
        http_client(),
        &server.server.url,
        token.expose(),
        Duration::from_secs(30),
        async |session| session.tools().await,
    )
    .await;
    match listed {
        Ok(tools) => {
            shared.gateway.remember(&server.server.name, &tools);
            shared.gateway.known(&server.server.name, tool)
        }
        Err(error) => {
            // Unknown annotations count as destructive (the protocol's default).
            tracing::warn!(%error, "cannot list the tools for their annotations");
            None
        }
    }
}

/// `tools/call`: checked, maybe approved by a person, forwarded and recorded.
async fn call(
    shared: &Shared,
    grant: &Grant,
    server: &ExtraMcp,
    token: &Secret<String>,
    headers: &HeaderMap,
    message: &Value,
    id: &Value,
) -> Response {
    let started = Instant::now();
    let tool = message
        .pointer("/params/name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let record = |decision, error: Option<String>| ToolCallRecord {
        run_id: Some(grant.run_id.clone()),
        command: None,
        person: grant.person.clone(),
        conversation: grant.event.conversation.clone(),
        server: server.server.name.clone(),
        tool: tool.clone(),
        decision,
        error,
        duration: started.elapsed(),
        at: shared.clock.now(),
    };
    if !permission::may_call(server, &grant.who, &tool) {
        tracing::warn!(mcp = %server.server.name, %tool, "tool call refused by the rules");
        audit(shared, record(ToolDecision::Denied, None)).await;
        return json_response(
            StatusCode::OK,
            None,
            &tool_error(id, "This tool is not available to this person here."),
        );
    }
    if server.rules.confirm != crate::config::Confirm::Never {
        let hints = annotations(shared, server, token, &tool).await;
        if permission::needs_confirmation(&server.rules, hints.as_ref()) {
            let arguments = message
                .pointer("/params/arguments")
                .cloned()
                .unwrap_or(Value::Null);
            let refusal = match crate::confirm::ask(shared, grant, server, &tool, &arguments).await
            {
                crate::confirm::Approval::Allowed => None,
                crate::confirm::Approval::Declined => Some((
                    None,
                    "The person did not approve this call. Do not retry it; tell them it was not done.",
                )),
                crate::confirm::Approval::Unreviewable => Some((
                    Some("too long to review".to_string()),
                    "This call is too long for a person to review, so it was not made. Use shorter arguments.",
                )),
            };
            if let Some((error, text)) = refusal {
                audit(shared, record(ToolDecision::Declined, error)).await;
                return json_response(StatusCode::OK, None, &tool_error(id, text));
            }
        }
    }
    let result = exchange(shared, grant, server, token, headers, message).await;
    let error = match &result {
        Err(e) => Some(e.to_string()),
        Ok(ex) if !reply_ok(ex.status) => Some(format!("HTTP {}", ex.status)),
        Ok(ex) => match &ex.response {
            None => Some("no response".into()),
            Some(r) if r.get("error").is_some() => Some(
                r.pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("error")
                    .to_string(),
            ),
            Some(r) if r.pointer("/result/isError") == Some(&Value::Bool(true)) => {
                Some("the tool reported an error".into())
            }
            Some(_) => None,
        },
    };
    tracing::info!(mcp = %server.server.name, %tool, ok = error.is_none(), "tool call");
    audit(shared, record(ToolDecision::Allowed, error)).await;
    pass(Some(id), result)
}

pub(crate) async fn audit(shared: &Shared, call: ToolCallRecord) {
    if let Err(error) = shared.store.record_tool_call(call).await {
        tracing::warn!(%error, "cannot record the tool call");
    }
}
