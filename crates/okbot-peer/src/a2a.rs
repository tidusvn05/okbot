//! The A2A subset okbot speaks (JSON-RPC binding): sending a message, getting and cancelling a
//! task, and the Agent Card, plus the okbot delegation extension.
//!
//! okbot sends A2A 1.0 (`SendMessage`, `GetTask`, `CancelTask`, `TASK_STATE_*`, parts without
//! `kind`, `A2A-Version: 1.0`) and answers 0.3 requests (`message/send`, `tasks/get`,
//! `tasks/cancel`) in 0.3's shapes, so older A2A clients keep working. Only this module knows A2A's
//! shapes; the rest of the crate uses okbot's types.

use okbot_core::{DelegationContext, PeerTaskState};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// The okbot delegation extension (required on every message between okbots).
pub const EXTENSION: &str =
    "https://github.com/tidusvn05/okbot/blob/main/docs/peers.md#delegation-v1";
/// Where the delegation context sits in a message's metadata.
pub const METADATA_KEY: &str = "okbot/delegation";
/// Where a finished task's token use sits in its status message's metadata.
pub const USAGE_KEY: &str = "okbot/usage";
/// The header naming the A2A version of a request.
pub const VERSION_HEADER: &str = "a2a-version";
/// The header naming the extensions a request uses.
pub const EXTENSIONS_HEADER: &str = "a2a-extensions";

/// An A2A protocol version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Version {
    /// 0.3.0: `message/send`, lowercase states, `kind` on parts.
    V03,
    /// 1.0: `SendMessage`, `TASK_STATE_*`, parts without `kind`.
    V10,
}

impl Version {
    /// The `A2A-Version` header value.
    pub fn header(self) -> &'static str {
        match self {
            Self::V03 => "0.3",
            Self::V10 => "1.0",
        }
    }

    /// From an `A2A-Version` header or a configuration value; missing means 0.3 (the spec's rule).
    pub fn parse(value: Option<&str>) -> Option<Self> {
        match value.map(str::trim) {
            None | Some("" | "0.3" | "0.3.0") => Some(Self::V03),
            Some("1.0" | "1.0.0" | "1") => Some(Self::V10),
            Some(_) => None,
        }
    }
}

/// An operation okbot offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// Send a message (a new task, or an answer to a task waiting for input).
    Send,
    /// Get a task.
    Get,
    /// Cancel a task.
    Cancel,
}

/// The JSON-RPC method of `op` in `version`.
pub fn method(op: Op, version: Version) -> &'static str {
    match (op, version) {
        (Op::Send, Version::V03) => "message/send",
        (Op::Get, Version::V03) => "tasks/get",
        (Op::Cancel, Version::V03) => "tasks/cancel",
        (Op::Send, Version::V10) => "SendMessage",
        (Op::Get, Version::V10) => "GetTask",
        (Op::Cancel, Version::V10) => "CancelTask",
    }
}

/// The operation and version a JSON-RPC method names.
pub fn op_of(method: &str) -> Option<(Op, Version)> {
    Some(match method {
        "message/send" => (Op::Send, Version::V03),
        "tasks/get" => (Op::Get, Version::V03),
        "tasks/cancel" => (Op::Cancel, Version::V03),
        "SendMessage" => (Op::Send, Version::V10),
        "GetTask" => (Op::Get, Version::V10),
        "CancelTask" => (Op::Cancel, Version::V10),
        _ => return None,
    })
}

/// JSON-RPC error codes.
pub mod code {
    /// Invalid JSON.
    pub const PARSE: i64 = -32700;
    /// Not a valid request.
    pub const INVALID_REQUEST: i64 = -32600;
    /// Unknown method.
    pub const METHOD_NOT_FOUND: i64 = -32601;
    /// Invalid parameters.
    pub const INVALID_PARAMS: i64 = -32602;
    /// A2A: no such task.
    pub const TASK_NOT_FOUND: i64 = -32001;
    /// A2A: the task cannot be cancelled.
    pub const NOT_CANCELABLE: i64 = -32002;
    /// A2A: the version is not supported.
    pub const VERSION_NOT_SUPPORTED: i64 = -32009;
    /// okbot: the request is refused (links, depth, deadline, limits).
    pub const REFUSED: i64 = -32010;
}

/// A JSON-RPC request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    /// `2.0`.
    pub jsonrpc: String,
    /// Request id.
    pub id: Value,
    /// Method.
    pub method: String,
    /// Parameters.
    #[serde(default)]
    pub params: Value,
}

impl Request {
    /// A request with a random id.
    pub fn new(method: &str, params: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: Value::String(hex::encode(rand::random::<[u8; 8]>())),
            method: method.into(),
            params,
        }
    }
}

/// A successful response.
pub fn result(id: &Value, result: &Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

/// An error response.
pub fn error(id: &Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// A message part.
#[derive(Debug, Clone, PartialEq)]
pub enum Part {
    /// Text.
    Text(String),
    /// Structured data.
    Data(Value),
}

impl Part {
    fn to_wire(&self, version: Version) -> Value {
        match (self, version) {
            (Self::Text(text), Version::V03) => json!({"kind": "text", "text": text}),
            (Self::Data(data), Version::V03) => json!({"kind": "data", "data": data}),
            (Self::Text(text), Version::V10) => json!({"text": text}),
            (Self::Data(data), Version::V10) => json!({"data": data}),
        }
    }

    /// Either version: the member present decides (1.0), which 0.3's `kind` agrees with.
    fn from_wire(value: &Value) -> Option<Self> {
        if let Some(text) = value.get("text").and_then(Value::as_str) {
            return Some(Self::Text(text.to_string()));
        }
        value.get("data").map(|d| Self::Data(d.clone()))
    }
}

/// An A2A message.
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    /// Unique per message (the idempotency key).
    pub message_id: String,
    /// The conversation.
    pub context_id: Option<String>,
    /// Continues this task (an answer to `input-required`).
    pub task_id: Option<String>,
    /// From the caller (`user`) rather than the agent.
    pub from_user: bool,
    /// Content.
    pub parts: Vec<Part>,
    /// Extension data.
    pub metadata: Option<Value>,
    /// Extensions this message uses.
    pub extensions: Vec<String>,
}

impl Message {
    /// A message from the caller.
    pub fn user(message_id: &str, context_id: &str, parts: Vec<Part>) -> Self {
        Self {
            message_id: message_id.into(),
            context_id: Some(context_id.into()),
            task_id: None,
            from_user: true,
            parts,
            metadata: None,
            extensions: vec![EXTENSION.into()],
        }
    }

    /// A message from the agent (the callee).
    pub fn agent(parts: Vec<Part>) -> Self {
        Self {
            message_id: hex::encode(rand::random::<[u8; 8]>()),
            context_id: None,
            task_id: None,
            from_user: false,
            parts,
            metadata: None,
            extensions: Vec::new(),
        }
    }

    /// The text parts, joined.
    pub fn text(&self) -> String {
        self.parts
            .iter()
            .filter_map(|p| match p {
                Part::Text(text) => Some(text.as_str()),
                Part::Data(_) => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The first data part.
    pub fn data(&self) -> Option<&Value> {
        self.parts.iter().find_map(|p| match p {
            Part::Data(data) => Some(data),
            Part::Text(_) => None,
        })
    }

    /// Attaches the delegation context.
    pub fn with_context(mut self, context: &DelegationContext) -> Self {
        self.metadata = Some(json!({ METADATA_KEY: context }));
        self
    }

    /// The delegation context, when present and valid.
    pub fn context(&self) -> Option<DelegationContext> {
        let value = self.metadata.as_ref()?.get(METADATA_KEY)?;
        serde_json::from_value(value.clone()).ok()
    }

    /// The message in `version`'s shape.
    pub fn to_wire(&self, version: Version) -> Value {
        let mut m = Map::new();
        if version == Version::V03 {
            m.insert("kind".into(), json!("message"));
        }
        let role = match (version, self.from_user) {
            (Version::V03, true) => "user",
            (Version::V03, false) => "agent",
            (Version::V10, true) => "ROLE_USER",
            (Version::V10, false) => "ROLE_AGENT",
        };
        m.insert("role".into(), json!(role));
        m.insert("messageId".into(), json!(self.message_id));
        if let Some(c) = &self.context_id {
            m.insert("contextId".into(), json!(c));
        }
        if let Some(t) = &self.task_id {
            m.insert("taskId".into(), json!(t));
        }
        m.insert(
            "parts".into(),
            Value::Array(self.parts.iter().map(|p| p.to_wire(version)).collect()),
        );
        if let Some(metadata) = &self.metadata {
            m.insert("metadata".into(), metadata.clone());
        }
        if !self.extensions.is_empty() {
            m.insert("extensions".into(), json!(self.extensions));
        }
        Value::Object(m)
    }

    /// A message in either version's shape.
    pub fn from_wire(value: &Value) -> Option<Self> {
        let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_string);
        Some(Self {
            message_id: text("messageId")?,
            context_id: text("contextId"),
            task_id: text("taskId"),
            from_user: matches!(text("role").as_deref(), Some("user" | "ROLE_USER")),
            parts: value
                .get("parts")?
                .as_array()?
                .iter()
                .filter_map(Part::from_wire)
                .collect(),
            metadata: value.get("metadata").cloned(),
            extensions: value
                .get("extensions")
                .and_then(Value::as_array)
                .map(|e| {
                    e.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
        })
    }
}

/// A task state in `version`'s spelling.
pub fn state_name(state: PeerTaskState, version: Version) -> &'static str {
    match version {
        Version::V03 => state.as_str(),
        Version::V10 => match state {
            PeerTaskState::Submitted => "TASK_STATE_SUBMITTED",
            PeerTaskState::Working => "TASK_STATE_WORKING",
            PeerTaskState::InputRequired => "TASK_STATE_INPUT_REQUIRED",
            PeerTaskState::Completed => "TASK_STATE_COMPLETED",
            PeerTaskState::Failed => "TASK_STATE_FAILED",
            PeerTaskState::Canceled => "TASK_STATE_CANCELED",
            PeerTaskState::Rejected => "TASK_STATE_REJECTED",
        },
    }
}

/// A task state in either spelling; an unknown one (`auth-required`, …) counts as failed.
pub fn parse_state(name: &str) -> PeerTaskState {
    let name = match name.strip_prefix("TASK_STATE_") {
        Some(rest) => rest.to_ascii_lowercase().replace('_', "-"),
        None => name.to_string(),
    };
    name.parse().unwrap_or(PeerTaskState::Failed)
}

/// An A2A task.
#[derive(Debug, Clone, PartialEq)]
pub struct Task {
    /// Task id.
    pub id: String,
    /// The conversation.
    pub context_id: String,
    /// Where it stands.
    pub state: PeerTaskState,
    /// The answer, the reason, the question, or progress.
    pub message: Option<Message>,
    /// When it changed (RFC 3339).
    pub timestamp: Option<String>,
}

impl Task {
    /// The task in `version`'s shape.
    pub fn to_wire(&self, version: Version) -> Value {
        let mut status = Map::new();
        status.insert("state".into(), json!(state_name(self.state, version)));
        if let Some(message) = &self.message {
            status.insert("message".into(), message.to_wire(version));
        }
        if let Some(t) = &self.timestamp {
            status.insert("timestamp".into(), json!(t));
        }
        let mut task = Map::new();
        if version == Version::V03 {
            task.insert("kind".into(), json!("task"));
        }
        task.insert("id".into(), json!(self.id));
        task.insert("contextId".into(), json!(self.context_id));
        task.insert("status".into(), Value::Object(status));
        Value::Object(task)
    }

    /// A task in either version's shape.
    pub fn from_wire(value: &Value) -> Option<Self> {
        let status = value.get("status")?;
        Some(Self {
            id: value.get("id")?.as_str()?.to_string(),
            context_id: value
                .get("contextId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            state: parse_state(status.get("state")?.as_str()?),
            message: status.get("message").and_then(Message::from_wire),
            timestamp: status
                .get("timestamp")
                .and_then(Value::as_str)
                .map(str::to_string),
        })
    }
}

/// The result of a send: 1.0 wraps the task (`{"task": …}`), 0.3 returns it.
pub fn send_result(task: &Task, version: Version) -> Value {
    match version {
        Version::V03 => task.to_wire(version),
        Version::V10 => json!({"task": task.to_wire(version)}),
    }
}

/// The task in a send result of either version.
pub fn unwrap_send_result(result: &Value) -> &Value {
    result.get("task").unwrap_or(result)
}

/// The Agent Card of a member for its team (A2A 1.0, with the 0.3 interface too).
pub fn agent_card(name: &str, description: &str, url: &str, skills: &[String]) -> Value {
    json!({
        "name": name,
        "description": description,
        "version": env!("CARGO_PKG_VERSION"),
        "supportedInterfaces": [
            {"url": url, "protocolBinding": "JSONRPC", "protocolVersion": "1.0"},
            {"url": url, "protocolBinding": "JSONRPC", "protocolVersion": "0.3"},
        ],
        "capabilities": {
            "streaming": false,
            "pushNotifications": false,
            "extensions": [{"uri": EXTENSION, "required": true,
                            "description": "okbot delegation context (origin, chain, deadline, budget, trace); requests are signed with OkbotPeer tokens"}],
        },
        "defaultInputModes": ["text/plain"],
        "defaultOutputModes": ["text/plain"],
        "skills": skills.iter().map(|s| json!({
            "id": s, "name": s, "description": s, "tags": [s],
        })).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use okbot_core::{Budget, Origin};

    fn context() -> DelegationContext {
        DelegationContext {
            origin: Origin {
                bot: "assistant".into(),
                conversation: "zalo:shop|-|123|-".into(),
                person: Some("lan".into()),
                name: "Lan".into(),
            },
            chain: vec!["assistant".into()],
            deadline: "2026-10-24T09:15:00Z".parse().unwrap(),
            budget: Budget {
                turns: 4,
                tokens: None,
            },
            trace: "4f1c2a9e0b7d3e61".into(),
        }
    }

    fn sample_task() -> Task {
        Task {
            id: "t-1".into(),
            context_id: "c-1".into(),
            state: PeerTaskState::InputRequired,
            message: Some(Message {
                message_id: "a-1".into(),
                ..Message::agent(vec![
                    Part::Text("Restart nginx?".into()),
                    Part::Data(
                        json!({"prompt": "p-1", "buttons": [{"id": "allow", "label": "Allow"}]}),
                    ),
                ])
            }),
            timestamp: Some("2026-10-24T09:00:00Z".into()),
        }
    }

    #[test]
    fn wire_format_in_both_versions() {
        let message = Message::user(
            "m-1",
            "c-1",
            vec![Part::Text("Find the release notes".into())],
        )
        .with_context(&context());
        for version in [Version::V10, Version::V03] {
            let mut request = Request::new(
                method(Op::Send, version),
                json!({"message": message.to_wire(version)}),
            );
            request.id = json!(1);
            let task = sample_task();
            let name = version.header().replace('.', "_");
            let result = send_result(&task, version);
            insta::with_settings!({sort_maps => true}, {
                insta::assert_json_snapshot!(format!("send_{name}"), request);
                insta::assert_json_snapshot!(format!("send_result_{name}"), result);
            });
            // Read back whatever the version.
            let parsed = Message::from_wire(&request.params["message"]).unwrap();
            assert_eq!(parsed, message);
            assert_eq!(parsed.context(), Some(context()));
            let back = Task::from_wire(unwrap_send_result(&result)).unwrap();
            assert_eq!(back, task);
        }
    }

    #[test]
    fn methods_states_and_versions() {
        for op in [Op::Send, Op::Get, Op::Cancel] {
            for version in [Version::V03, Version::V10] {
                assert_eq!(op_of(method(op, version)), Some((op, version)));
            }
        }
        assert_eq!(op_of("tasks/resubscribe"), None);
        for state in [
            PeerTaskState::Submitted,
            PeerTaskState::Working,
            PeerTaskState::InputRequired,
            PeerTaskState::Completed,
            PeerTaskState::Failed,
            PeerTaskState::Canceled,
            PeerTaskState::Rejected,
        ] {
            for version in [Version::V03, Version::V10] {
                assert_eq!(parse_state(state_name(state, version)), state);
            }
        }
        assert_eq!(
            parse_state("TASK_STATE_AUTH_REQUIRED"),
            PeerTaskState::Failed
        );
        assert_eq!(
            Version::parse(None),
            Some(Version::V03),
            "missing means 0.3"
        );
        assert_eq!(Version::parse(Some("1.0")), Some(Version::V10));
        assert_eq!(Version::parse(Some("2.0")), None);
    }

    #[test]
    fn agent_card() {
        let mut card = super::agent_card(
            "ops",
            "Operations",
            "https://ops.example.com/a2a/acme",
            &["infrastructure".into()],
        );
        card["version"] = json!("[version]");
        insta::with_settings!({sort_maps => true}, {
            insta::assert_json_snapshot!("agent_card", card);
        });
    }

    #[test]
    fn a_message_without_context_has_none() {
        let message = Message::user("m", "c", Vec::new());
        assert_eq!(message.context(), None);
        let mut bad = message;
        bad.metadata = Some(json!({METADATA_KEY: {"chain": "x"}}));
        assert_eq!(bad.context(), None);
    }
}
