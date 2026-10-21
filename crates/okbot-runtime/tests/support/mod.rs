//! A stand-in MCP server and an agent that calls tools through the gateway, for end-to-end tests.
#![allow(dead_code, clippy::unwrap_used)] // shared by several test crates; a panic is the failure report

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use okbot_core::{
    AgentBackend, AgentCaps, AgentError, AgentReply, AgentTurn, ToolPolicy, async_trait,
};
use serde_json::{Value, json};

/// One `tools/call` the server received.
#[derive(Debug, Clone)]
pub(crate) struct Received {
    /// `Authorization` header.
    pub(crate) bearer: String,
    /// Tool name.
    pub(crate) tool: String,
    /// Arguments.
    pub(crate) arguments: Value,
}

/// A stand-in MCP server at `http://<addr>/mcp` listing `tools` (each with its annotations) and
/// answering every call with `"<tool> ok"`. Replies come as event streams that stay open, as some
/// servers send them.
pub(crate) async fn mcp_server(tools: Value) -> (SocketAddr, Arc<Mutex<Vec<Received>>>) {
    mcp_server_with(tools, axum::Router::new()).await
}

/// [`mcp_server`] with more routes on the same origin (prompt callbacks).
pub(crate) async fn mcp_server_with(
    tools: Value,
    extra: axum::Router,
) -> (SocketAddr, Arc<Mutex<Vec<Received>>>) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let state = calls.clone();
    let handler = move |headers: HeaderMap, body: String| {
        let state = state.clone();
        let tools = tools.clone();
        async move {
            let bearer = headers
                .get(header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_string();
            let msg: Value = serde_json::from_str(&body).unwrap();
            let Some(id) = msg.get("id").cloned() else {
                return StatusCode::ACCEPTED.into_response();
            };
            let result = match msg["method"].as_str().unwrap() {
                "initialize" => {
                    json!({"protocolVersion": "2025-06-18", "capabilities": {"tools": {}},
                                       "serverInfo": {"name": "fake", "version": "0"}})
                }
                "tools/list" => json!({"tools": tools}),
                "tools/call" => {
                    let tool = msg["params"]["name"].as_str().unwrap().to_string();
                    state.lock().unwrap().push(Received {
                        bearer,
                        tool: tool.clone(),
                        arguments: msg["params"]["arguments"].clone(),
                    });
                    json!({"content": [{"type": "text", "text": format!("{tool} ok")}]})
                }
                "ping" => json!({}),
                other => panic!("unexpected {other}"),
            };
            let reply = json!({"jsonrpc": "2.0", "id": id, "result": result});
            let progress = json!({"jsonrpc": "2.0", "method": "notifications/progress",
                                  "params": {"progressToken": 1, "progress": 1}});
            // The stream stays open after the answer: the reader must stop at the response.
            let stream = futures_stream(format!(
                "event: message\ndata: {progress}\n\nevent: message\ndata: {reply}\n\n"
            ));
            (
                [
                    (header::CONTENT_TYPE, "text/event-stream"),
                    (header::HeaderName::from_static("mcp-session-id"), "s1"),
                ],
                axum::body::Body::from_stream(stream),
            )
                .into_response()
        }
    };
    let app = axum::Router::new()
        .route(
            "/mcp",
            axum::routing::post(handler).delete(|| async { StatusCode::OK }),
        )
        .merge(extra);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (addr, calls)
}

/// A body that sends `text` and then never ends.
fn futures_stream(
    text: String,
) -> impl futures::Stream<Item = Result<axum::body::Bytes, std::io::Error>> + Send {
    use futures::StreamExt as _;
    futures::stream::once(async move { Ok(axum::body::Bytes::from(text)) })
        .chain(futures::stream::pending())
}

/// One step of a [`ToolAgent`] run.
#[derive(Debug, Clone)]
pub(crate) enum Step {
    /// `tools/list` on a server; the names are recorded.
    List(&'static str),
    /// `tools/call` of a tool with arguments; the result text (or error) is recorded.
    Call(&'static str, &'static str, Value),
    /// Any JSON-RPC message; the reply (or the HTTP status) is recorded.
    Raw(&'static str, Value),
}

/// An agent that talks MCP to the servers of its turn, as Claude or Codex would.
pub(crate) struct ToolAgent {
    steps: Vec<Step>,
    /// What each step returned, in order.
    pub(crate) results: Mutex<Vec<Value>>,
    /// The turns it ran.
    pub(crate) turns: Mutex<Vec<AgentTurn>>,
}

impl ToolAgent {
    pub(crate) fn new(steps: Vec<Step>) -> Self {
        Self {
            steps,
            results: Mutex::new(Vec::new()),
            turns: Mutex::new(Vec::new()),
        }
    }

    pub(crate) fn results(&self) -> Vec<Value> {
        self.results.lock().unwrap().clone()
    }

    pub(crate) fn turns(&self) -> Vec<AgentTurn> {
        self.turns.lock().unwrap().clone()
    }
}

/// POSTs one JSON-RPC message with the turn's token; the response message (or the HTTP status).
pub(crate) async fn rpc(url: &str, token: &str, message: &Value) -> Result<Value, u16> {
    rpc_in(url, token, None, message)
        .await
        .map(|(reply, _)| reply)
}

/// [`rpc`] within a session; the reply and the session id the server gave.
pub(crate) async fn rpc_in(
    url: &str,
    token: &str,
    session: Option<&str>,
    message: &Value,
) -> Result<(Value, Option<String>), u16> {
    let mut request = reqwest::Client::new()
        .post(url)
        .bearer_auth(token)
        .header("accept", "application/json, text/event-stream")
        .json(message)
        .timeout(Duration::from_secs(30));
    if let Some(id) = session {
        request = request.header("mcp-session-id", id);
    }
    let reply = request.send().await.map_err(|_| 0u16)?;
    if !reply.status().is_success() {
        return Err(reply.status().as_u16());
    }
    let session = reply
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let text = reply.text().await.unwrap_or_default();
    Ok((serde_json::from_str(&text).unwrap_or(Value::Null), session))
}

/// Opens a session as an MCP client does: `initialize`, then `notifications/initialized`.
async fn open(url: &str, token: &str) -> Result<Option<String>, u16> {
    let init = json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {},
        "clientInfo": {"name": "tool-agent", "version": "0"}}});
    let (_, session) = rpc_in(url, token, None, &init).await?;
    let ready = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    rpc_in(url, token, session.as_deref(), &ready).await?;
    Ok(session)
}

#[async_trait]
impl AgentBackend for ToolAgent {
    fn name(&self) -> &'static str {
        "tools"
    }

    fn caps(&self) -> AgentCaps {
        AgentCaps {
            mcp: true,
            tool_policies: vec![ToolPolicy::Web, ToolPolicy::McpOnly],
            ..AgentCaps::default()
        }
    }

    async fn run(&self, turn: AgentTurn) -> Result<AgentReply, AgentError> {
        self.turns.lock().unwrap().push(turn.clone());
        let server = |name: &str| {
            let s = turn.mcp.iter().find(|s| s.name == name)?;
            let token = turn
                .env
                .iter()
                .find(|(k, _)| *k == s.bearer_token_env)?
                .1
                .expose()
                .clone();
            Some((s.url.clone(), token))
        };
        let mut out = Vec::new();
        let mut sessions: std::collections::HashMap<String, Option<String>> =
            std::collections::HashMap::new();
        for step in &self.steps {
            let name = match step {
                Step::List(name) | Step::Call(name, ..) | Step::Raw(name, _) => *name,
            };
            let session = match server(name) {
                Some((url, token)) if !sessions.contains_key(name) => {
                    let opened = open(&url, &token).await.ok().flatten();
                    sessions.insert(name.to_string(), opened);
                    sessions[name].clone()
                }
                _ => sessions.get(name).cloned().flatten(),
            };
            let session = session.as_deref();
            let result = match step {
                Step::List(name) => match server(name) {
                    None => json!("not offered"),
                    Some((url, token)) => {
                        match rpc_in(
                            &url,
                            &token,
                            session,
                            &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
                        )
                        .await
                        .map(|(reply, _)| reply)
                        {
                            Ok(reply) => json!(
                                reply["result"]["tools"]
                                    .as_array()
                                    .into_iter()
                                    .flatten()
                                    .filter_map(|t| t["name"].as_str())
                                    .collect::<Vec<_>>()
                            ),
                            Err(status) => json!(status),
                        }
                    }
                },
                Step::Call(name, tool, arguments) => match server(name) {
                    None => json!("not offered"),
                    Some((url, token)) => {
                        let message = json!({"jsonrpc": "2.0", "id": "c-1", "method": "tools/call",
                                             "params": {"name": tool, "arguments": arguments}});
                        match rpc_in(&url, &token, session, &message).await {
                            Ok((reply, _)) => reply["result"]["content"][0]["text"].clone(),
                            Err(status) => json!(status),
                        }
                    }
                },
                Step::Raw(name, message) => match server(name) {
                    None => json!("not offered"),
                    Some((url, token)) => match rpc_in(&url, &token, session, message).await {
                        Ok((reply, _)) => reply,
                        Err(status) => json!(status),
                    },
                },
            };
            out.push(result.to_string());
            self.results.lock().unwrap().push(result);
        }
        Ok(AgentReply {
            text: out.join(" | "),
            backend: "tools".into(),
            model: None,
            session_id: None,
            usage: Some(okbot_core::TokenUsage {
                input: 100,
                output: 10,
                ..okbot_core::TokenUsage::default()
            }),
            duration: Duration::from_millis(1),
        })
    }
}
