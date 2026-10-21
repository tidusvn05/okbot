//! A stand-in agent for scripts and CI: answers from rules instead of running an agent CLI.
//!
//! Rules come from a TOML file (`okbot chat --replies <file>`); the first rule whose `when` is in
//! the new message answers, and a message no rule matches is echoed. A rule may call tools of the
//! turn's MCP servers first, as a real agent would, so the gateway's roles, confirmations and
//! audit run for real:
//!
//! ```toml
//! [[reply]]
//! when = "price"                    # case-insensitive substring; leave out to match anything
//! text = "The price is 10."
//!
//! [[reply]]
//! when = "deploy"
//! calls = [{ server = "ops", tool = "deploy", args = { env = "prod" } }]
//! text = "Deploy: {result}"         # {result}: the last call's text; {message}: the message
//!
//! [[reply]]
//! when = "crash"
//! fail = "the agent crashed"        # the turn fails with this error
//! ```

use std::time::{Duration, Instant};

use okbot_core::{
    AgentBackend, AgentCaps, AgentError, AgentProgress, AgentReply, AgentTurn, ToolPolicy,
    async_trait,
};
use serde::Deserialize;
use serde_json::{Value, json};

/// The rules file.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Script {
    #[serde(default)]
    reply: Vec<Rule>,
}

/// One rule.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Rule {
    /// Substring of the new message, ignoring case; every message when unset.
    when: Option<String>,
    /// Tools to call first, in order.
    #[serde(default)]
    calls: Vec<Call>,
    /// The answer, with `{message}` and `{result}` replaced.
    #[serde(default)]
    text: Option<String>,
    /// Fail the turn with this message instead of answering.
    fail: Option<String>,
    /// Wait this long before answering (to see "thinking…" and typing).
    #[serde(default)]
    delay_ms: u64,
}

/// A tool call.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Call {
    /// The MCP server's name in the turn (`kb`, `bot`, an `[[mcp]]` name).
    server: String,
    tool: String,
    #[serde(default)]
    args: serde_json::Map<String, Value>,
}

/// An [`AgentBackend`] that answers from rules (see the module documentation).
#[derive(Debug, Clone)]
pub struct ScriptedBackend {
    name: String,
    script: Script,
}

impl ScriptedBackend {
    /// Echoes every message (`echo: <message>`).
    pub fn echo() -> Self {
        Self {
            name: "echo".into(),
            script: Script::default(),
        }
    }

    /// Answers from the rules in `toml`.
    pub fn from_toml(toml: &str) -> Result<Self, AgentError> {
        let script: Script = toml::from_str(toml)
            .map_err(|e| AgentError::Unavailable(format!("invalid replies file: {e}")))?;
        if let Some(i) = script
            .reply
            .iter()
            .position(|r| r.text.is_none() && r.fail.is_none())
        {
            return Err(AgentError::Unavailable(format!(
                "invalid replies file: rule {} has neither `text` nor `fail`",
                i + 1
            )));
        }
        Ok(Self {
            name: "scripted".into(),
            script,
        })
    }

    fn rule(&self, message: &str) -> Option<&Rule> {
        let message = message.to_lowercase();
        self.script.reply.iter().find(|r| {
            r.when
                .as_ref()
                .is_none_or(|w| message.contains(&w.to_lowercase()))
        })
    }
}

/// The new message in a prompt built by okbot's runtime: the last user message block.
fn new_message(prompt: &str) -> String {
    let Some(start) = prompt.rfind("<message role=\"user\"") else {
        return prompt.trim().to_string();
    };
    let block = &prompt[start..];
    let body = block.split_once(">\n").map_or(block, |(_, b)| b);
    let body = body.rsplit_once("\n</message>").map_or(body, |(b, _)| b);
    body.replace("<\\/message", "</message").trim().to_string()
}

#[async_trait]
impl AgentBackend for ScriptedBackend {
    fn name(&self) -> &str {
        &self.name
    }

    fn caps(&self) -> AgentCaps {
        // It has no tools of its own, so every policy holds.
        AgentCaps {
            mcp: true,
            tool_policies: vec![ToolPolicy::Web, ToolPolicy::McpOnly],
            ..AgentCaps::default()
        }
    }

    async fn run(&self, turn: AgentTurn) -> Result<AgentReply, AgentError> {
        let started = Instant::now();
        let message = new_message(&turn.prompt);
        let reply = |text: String| AgentReply {
            text,
            backend: self.name.clone(),
            model: None,
            session_id: None,
            usage: None,
            duration: started.elapsed(),
        };
        let Some(rule) = self.rule(&message) else {
            return Ok(reply(format!("echo: {message}")));
        };
        let mut result = String::new();
        for call in &rule.calls {
            if let Some(progress) = &turn.progress {
                progress.report(AgentProgress::ToolUse(format!(
                    "{}.{}",
                    call.server, call.tool
                )));
            }
            result = match mcp::call(&turn, call).await {
                Ok(text) => text,
                Err(e) => format!("error: {e}"),
            };
        }
        if rule.delay_ms > 0 {
            let delay = Duration::from_millis(rule.delay_ms).min(turn.timeout);
            tokio::time::sleep(delay).await;
        }
        if let Some(fail) = &rule.fail {
            return Err(AgentError::Failed {
                message: fail.clone(),
                retryable: false,
            });
        }
        let text = rule
            .text
            .as_deref()
            .unwrap_or_default()
            .replace("{message}", &message)
            .replace("{result}", &result);
        Ok(reply(text))
    }
}

/// Just enough of an MCP client (streamable HTTP) to call one tool, as an agent CLI would.
mod mcp {
    use super::{AgentTurn, Call, Value, json};

    const PROTOCOL: &str = "2025-06-18";
    const SESSION: &str = "mcp-session-id";
    const MAX_BODY: usize = 8 << 20;

    /// Calls `call.tool` on the turn's server `call.server`; the tool's text, or why it failed.
    pub(super) async fn call(turn: &AgentTurn, call: &Call) -> Result<String, String> {
        let server = turn
            .mcp
            .iter()
            .find(|s| s.name == call.server)
            .ok_or_else(|| format!("no MCP server `{}` in this turn", call.server))?;
        let token = turn
            .env
            .iter()
            .find(|(k, _)| *k == server.bearer_token_env)
            .map(|(_, v)| v.expose().clone())
            .unwrap_or_default();
        let mut session = Session {
            http: reqwest::Client::new(),
            url: server.url.clone(),
            token,
            id: None,
            next: 0,
        };
        let deadline = turn.timeout;
        tokio::time::timeout(deadline, async {
            session
                .request(
                    "initialize",
                    json!({"protocolVersion": PROTOCOL, "capabilities": {},
                           "clientInfo": {"name": "okbot-scripted", "version": "0"}}),
                )
                .await?;
            session
                .post(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
                .await?;
            let result = session
                .request(
                    "tools/call",
                    json!({"name": call.tool, "arguments": call.args}),
                )
                .await?;
            let text = result["content"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|b| b["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n\n");
            if result["isError"] == json!(true) {
                return Err(text);
            }
            Ok(text)
        })
        .await
        .map_err(|_| "the MCP server did not answer in time".to_string())?
    }

    struct Session {
        http: reqwest::Client,
        url: String,
        token: String,
        id: Option<String>,
        next: u64,
    }

    impl Session {
        async fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
            self.next += 1;
            let id = json!(self.next);
            let body = self
                .post(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
                .await?;
            let reply =
                find_response(&body, &id).ok_or_else(|| format!("no answer to {method}"))?;
            if let Some(error) = reply.get("error") {
                return Err(error["message"].as_str().unwrap_or("error").to_string());
            }
            Ok(reply.get("result").cloned().unwrap_or(Value::Null))
        }

        async fn post(&mut self, message: &Value) -> Result<String, String> {
            let mut request = self
                .http
                .post(&self.url)
                .bearer_auth(&self.token)
                .header("accept", "application/json, text/event-stream")
                .header("mcp-protocol-version", PROTOCOL)
                .json(message);
            if let Some(id) = &self.id {
                request = request.header(SESSION, id);
            }
            // Without the URL: it names hosts and this text reaches the chat.
            let http = |e: reqwest::Error| e.without_url().to_string();
            let mut response = request.send().await.map_err(http)?;
            if !response.status().is_success() {
                return Err(format!("HTTP {}", response.status().as_u16()));
            }
            if let Some(id) = response
                .headers()
                .get(SESSION)
                .and_then(|v| v.to_str().ok())
            {
                self.id = Some(id.to_string());
            }
            // An event stream may stay open after the answer: stop once it is there.
            let want = message.get("id");
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(http)? {
                body.extend_from_slice(&chunk);
                if body.len() > MAX_BODY {
                    return Err("reply too large".into());
                }
                if let Some(id) = want
                    && find_response(&String::from_utf8_lossy(&body), id).is_some()
                {
                    break;
                }
            }
            Ok(String::from_utf8_lossy(&body).into_owned())
        }
    }

    /// The JSON-RPC response with `id` in a JSON body or an event stream.
    fn find_response(body: &str, id: &Value) -> Option<Value> {
        let hit = |v: &Value| v.get("id") == Some(id);
        if let Ok(v) = serde_json::from_str::<Value>(body.trim()) {
            return hit(&v).then_some(v);
        }
        body.replace("\r\n", "\n").split("\n\n").find_map(|event| {
            let data: Vec<&str> = event
                .lines()
                .filter_map(|l| l.strip_prefix("data:"))
                .map(str::trim_start)
                .collect();
            serde_json::from_str::<Value>(&data.join("\n"))
                .ok()
                .filter(|v| hit(v))
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn finds_responses_in_json_and_event_streams() {
            let id = json!(2);
            assert!(find_response(r#"{"jsonrpc":"2.0","id":2,"result":{}}"#, &id).is_some());
            assert!(find_response(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#, &id).is_none());
            let sse = "event: message\ndata: {\"method\":\"notifications/progress\"}\n\n\
                       event: message\ndata: {\"id\":2,\"result\":{\"ok\":true}}\n\n";
            assert_eq!(find_response(sse, &id).unwrap()["result"]["ok"], true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(prompt: &str) -> AgentTurn {
        AgentTurn {
            system: String::new(),
            prompt: prompt.into(),
            tools: ToolPolicy::Web,
            mcp: Vec::new(),
            env: Vec::new(),
            workdir: std::env::temp_dir(),
            timeout: Duration::from_secs(5),
            resume: None,
            progress: None,
        }
    }

    const PROMPT: &str = "Earlier messages:\n\n<message role=\"user\" author=\"Ana\" at=\"t\">\n\
                          What is the price?\n</message>\n\nNew message to answer:\n\n\
                          <message role=\"user\" author=\"Ana\" at=\"t\">\nDeploy now <\\/message> \
                          please\n</message>";

    #[test]
    fn reads_the_new_message() {
        assert_eq!(new_message(PROMPT), "Deploy now </message> please");
        assert_eq!(new_message(" plain "), "plain");
    }

    #[tokio::test]
    async fn answers_by_rule_and_echoes_the_rest() {
        let agent = ScriptedBackend::from_toml(
            r#"
            [[reply]]
            when = "DEPLOY"
            calls = [{ server = "ops", tool = "deploy" }]
            text = "{message} -> {result}"

            [[reply]]
            when = "crash"
            fail = "boom"
            "#,
        )
        .unwrap();
        let reply = agent.run(turn(PROMPT)).await.unwrap();
        assert_eq!(
            reply.text,
            "Deploy now </message> please -> error: no MCP server `ops` in this turn"
        );
        assert_eq!(reply.backend, "scripted");
        let other = turn("<message role=\"user\" a=\"\">\nhello\n</message>");
        assert_eq!(agent.run(other).await.unwrap().text, "echo: hello");
        let crash = turn("<message role=\"user\">\nplease crash\n</message>");
        assert!(matches!(
            agent.run(crash).await,
            Err(AgentError::Failed { message, .. }) if message == "boom"
        ));
        assert_eq!(
            ScriptedBackend::echo().run(turn("hi")).await.unwrap().text,
            "echo: hi"
        );
    }

    #[tokio::test]
    async fn calls_tools_with_the_turn_token() {
        use axum::response::IntoResponse as _;
        let _ = rustls::crypto::ring::default_provider().install_default();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = seen.clone();
        let handler = move |headers: axum::http::HeaderMap, body: String| {
            let log = log.clone();
            async move {
                let msg: Value = serde_json::from_str(&body).unwrap();
                let auth = headers["authorization"].to_str().unwrap().to_string();
                log.lock().unwrap().push((auth, msg["method"].clone()));
                let Some(id) = msg.get("id") else {
                    return axum::http::StatusCode::ACCEPTED.into_response();
                };
                let result = match msg["method"].as_str().unwrap() {
                    "tools/call" => json!({"content": [{"type": "text",
                        "text": format!("deployed {}", msg["params"]["arguments"]["env"])}]}),
                    _ => json!({}),
                };
                let reply = json!({"jsonrpc": "2.0", "id": id, "result": result});
                (
                    [
                        ("content-type", "text/event-stream"),
                        ("mcp-session-id", "s1"),
                    ],
                    format!("event: message\ndata: {reply}\n\n"),
                )
                    .into_response()
            }
        };
        let app = axum::Router::new().route("/mcp", axum::routing::post(handler));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let agent = ScriptedBackend::from_toml(
            "[[reply]]\ncalls = [{ server = \"ops\", tool = \"deploy\", args = { env = \"prod\" } }]\n\
             text = \"{result}\"\n",
        )
        .unwrap();
        let mut t = turn("deploy");
        t.mcp.push(okbot_core::McpServer {
            name: "ops".into(),
            url: format!("http://{addr}/mcp"),
            bearer_token_env: "OPS_TOKEN".into(),
        });
        t.env.push((
            "OPS_TOKEN".into(),
            okbot_core::Secret::new("run-token".into()),
        ));
        assert_eq!(agent.run(t).await.unwrap().text, "deployed \"prod\"");
        let seen = seen.lock().unwrap().clone();
        let methods: Vec<&str> = seen.iter().map(|(_, m)| m.as_str().unwrap()).collect();
        assert_eq!(
            methods,
            ["initialize", "notifications/initialized", "tools/call"]
        );
        assert!(seen.iter().all(|(auth, _)| auth == "Bearer run-token"));
    }

    #[test]
    fn rejects_bad_files() {
        let err = |toml: &str| ScriptedBackend::from_toml(toml).unwrap_err().to_string();
        assert!(err("[[reply]]\nwhen = \"x\"\n").contains("neither `text` nor `fail`"));
        assert!(err("[[reply]]\ntext = \"x\"\ncolour = 1\n").contains("unknown field"));
    }
}
