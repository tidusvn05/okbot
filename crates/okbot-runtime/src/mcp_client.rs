//! A small MCP client (streamable HTTP) for `[[command]]`: one session to list the tools and call
//! one. Responses may be plain JSON or a server-sent event stream; both are read.

use std::time::Duration;

use serde_json::{Value, json};

const PROTOCOL: &str = "2025-06-18";
const SESSION: &str = "mcp-session-id";
/// Larger replies are refused rather than buffered.
const MAX_BODY: usize = 4 << 20;

/// Calling the server failed (not the tool: a tool's own error is [`ToolOutput::is_error`]).
#[derive(Debug, thiserror::Error)]
pub(crate) enum McpError {
    #[error("the server did not answer in time")]
    Timeout,
    #[error("the server cannot be reached: {0}")]
    Http(String),
    #[error("the server refused the request (HTTP {0})")]
    Status(u16),
    #[error("{0}")]
    Rpc(String),
    #[error("unexpected reply: {0}")]
    Protocol(String),
}

/// A tool's result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolOutput {
    /// The text blocks, joined.
    pub(crate) text: String,
    /// The tool reported a failure.
    pub(crate) is_error: bool,
}

/// One session with an MCP server.
pub(crate) struct Session {
    http: reqwest::Client,
    url: String,
    bearer: String,
    id: Option<String>,
    protocol: String,
    next_id: u64,
}

impl Session {
    /// Initializes a session.
    pub(crate) async fn open(
        http: reqwest::Client,
        url: &str,
        bearer: &str,
    ) -> Result<Self, McpError> {
        let mut s = Self {
            http,
            url: url.to_string(),
            bearer: bearer.to_string(),
            id: None,
            protocol: PROTOCOL.to_string(),
            next_id: 0,
        };
        let init = s
            .request(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL,
                    "capabilities": {},
                    "clientInfo": {"name": "okbot", "version": env!("CARGO_PKG_VERSION")},
                }),
            )
            .await?;
        if let Some(v) = init.get("protocolVersion").and_then(Value::as_str) {
            s.protocol = v.to_string();
        }
        s.post(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .await?;
        Ok(s)
    }

    /// Every tool's input schema, by name (`tools/list`, all pages).
    pub(crate) async fn tool_schema(&mut self, tool: &str) -> Result<Option<Value>, McpError> {
        let mut cursor: Option<String> = None;
        for _ in 0..20 {
            let params = match &cursor {
                Some(c) => json!({"cursor": c}),
                None => json!({}),
            };
            let page = self.request("tools/list", params).await?;
            let tools = page.get("tools").and_then(Value::as_array);
            if let Some(found) = tools
                .into_iter()
                .flatten()
                .find(|t| t.get("name").and_then(Value::as_str) == Some(tool))
            {
                return Ok(Some(found.get("inputSchema").cloned().unwrap_or(json!({}))));
            }
            match page.get("nextCursor").and_then(Value::as_str) {
                Some(c) if !c.is_empty() => cursor = Some(c.to_string()),
                _ => return Ok(None),
            }
        }
        Ok(None)
    }

    /// `tools/call`.
    pub(crate) async fn call(
        &mut self,
        tool: &str,
        arguments: serde_json::Map<String, Value>,
    ) -> Result<ToolOutput, McpError> {
        let result = self
            .request("tools/call", json!({"name": tool, "arguments": arguments}))
            .await?;
        let text = result
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n\n");
        Ok(ToolOutput {
            text,
            is_error: result.get("isError").and_then(Value::as_bool) == Some(true),
        })
    }

    /// Ends the session (best effort).
    pub(crate) async fn close(self) {
        if let Some(id) = &self.id {
            let _ = self
                .http
                .delete(&self.url)
                .bearer_auth(&self.bearer)
                .header(SESSION, id)
                .timeout(Duration::from_secs(5))
                .send()
                .await;
        }
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value, McpError> {
        self.next_id += 1;
        let id = self.next_id;
        let reply = self
            .post(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await?;
        let message = find_response(&reply, id)
            .ok_or_else(|| McpError::Protocol(format!("no answer to {method}")))?;
        if let Some(error) = message.get("error") {
            let text = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("error");
            return Err(McpError::Rpc(text.to_string()));
        }
        Ok(message.get("result").cloned().unwrap_or(Value::Null))
    }

    /// POSTs one message; the reply body (empty for notifications).
    async fn post(&mut self, message: &Value) -> Result<String, McpError> {
        let mut req = self
            .http
            .post(&self.url)
            .bearer_auth(&self.bearer)
            .header("accept", "application/json, text/event-stream")
            .header("mcp-protocol-version", &self.protocol)
            .json(message);
        if let Some(id) = &self.id {
            req = req.header(SESSION, id);
        }
        let mut resp = req
            .send()
            .await
            .map_err(|e| McpError::Http(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(McpError::Status(resp.status().as_u16()));
        }
        if let Some(id) = resp.headers().get(SESSION).and_then(|v| v.to_str().ok()) {
            self.id = Some(id.to_string());
        }
        let sse = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("text/event-stream"));
        let want = message.get("id").cloned();
        let mut body = Vec::new();
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| McpError::Http(e.to_string()))?
        {
            body.extend_from_slice(&chunk);
            if body.len() > MAX_BODY {
                return Err(McpError::Protocol("reply too large".into()));
            }
            // An event stream may stay open after the answer: stop once it is there.
            if sse
                && let Some(id) = want.as_ref().and_then(Value::as_u64)
                && find_response(&String::from_utf8_lossy(&body), id).is_some()
            {
                break;
            }
        }
        Ok(String::from_utf8_lossy(&body).into_owned())
    }
}

/// The JSON-RPC response with `id` in a JSON body or an event stream.
fn find_response(body: &str, id: u64) -> Option<Value> {
    let matches = |v: &Value| {
        v.get("id").and_then(Value::as_u64) == Some(id)
            && (v.get("result").is_some() || v.get("error").is_some())
    };
    let pick = |v: Value| match v {
        Value::Array(items) => items.into_iter().find(|m| matches(m)),
        v if matches(&v) => Some(v),
        _ => None,
    };
    if let Ok(v) = serde_json::from_str::<Value>(body.trim()) {
        return pick(v);
    }
    // Server-sent events: `data:` lines, one event per blank-line-separated block.
    let body = body.replace("\r\n", "\n");
    for event in body.split("\n\n") {
        let data: Vec<&str> = event
            .lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .map(|d| d.strip_prefix(' ').unwrap_or(d))
            .collect();
        if data.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(&data.join("\n"))
            && let Some(found) = pick(v)
        {
            return Some(found);
        }
    }
    None
}

/// Runs `f` against a fresh session with an overall time limit, closing the session afterwards.
pub(crate) async fn with_session<T>(
    http: reqwest::Client,
    url: &str,
    bearer: &str,
    limit: Duration,
    f: impl AsyncFnOnce(&mut Session) -> Result<T, McpError>,
) -> Result<T, McpError> {
    tokio::time::timeout(limit, async {
        let mut session = Session::open(http, url, bearer).await?;
        let out = f(&mut session).await;
        session.close().await;
        out
    })
    .await
    .map_err(|_| McpError::Timeout)?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_response_in_json_and_events() {
        let json = r#"{"jsonrpc":"2.0","id":2,"result":{"ok":true}}"#;
        assert_eq!(find_response(json, 2).unwrap()["result"]["ok"], true);
        assert!(find_response(json, 3).is_none());
        let sse = "event: message\r\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\r\n\r\n\
                   id: 7\ndata: {\"jsonrpc\":\"2.0\",\"id\":2,\n\
                   data: \"error\":{\"code\":-1,\"message\":\"no\"}}\n\n";
        assert_eq!(find_response(sse, 2).unwrap()["error"]["message"], "no");
        assert!(
            find_response("data: {\"id\":2}\n\n", 2).is_none(),
            "not a response"
        );
    }
}
