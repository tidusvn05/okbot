//! The A2A 0.3.0 subset okbot speaks (JSON-RPC binding): `message/send`, `tasks/get`,
//! `tasks/cancel` and the Agent Card, plus the okbot delegation extension.
//!
//! Only this module knows A2A's shapes; the rest of the crate uses okbot's types.

use okbot_core::{DelegationContext, PeerTaskState};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The A2A version spoken.
pub const PROTOCOL_VERSION: &str = "0.3.0";
/// The okbot delegation extension (required on every message).
pub const EXTENSION: &str =
    "https://github.com/tidusvn05/okbot/blob/main/docs/peers.md#delegation-v1";
/// Where the delegation context sits in a message's metadata.
pub const METADATA_KEY: &str = "okbot/delegation";
/// Where a finished task's token use sits in its status message's metadata.
pub const USAGE_KEY: &str = "okbot/usage";

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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Part {
    /// Text.
    Text {
        /// The text.
        text: String,
    },
    /// Structured data.
    Data {
        /// The data.
        data: Value,
    },
}

/// An A2A message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    /// Always `message`.
    pub kind: String,
    /// `user` (from the caller) or `agent`.
    pub role: String,
    /// Unique per message (the idempotency key).
    pub message_id: String,
    /// The conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_id: Option<String>,
    /// Continues this task (an answer to `input-required`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// Content.
    pub parts: Vec<Part>,
    /// Extension data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    /// Extensions this message uses.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<String>,
}

impl Message {
    /// A message from the caller.
    pub fn user(message_id: &str, context_id: &str, parts: Vec<Part>) -> Self {
        Self {
            kind: "message".into(),
            role: "user".into(),
            message_id: message_id.into(),
            context_id: Some(context_id.into()),
            task_id: None,
            parts,
            metadata: None,
            extensions: vec![EXTENSION.into()],
        }
    }

    /// A message from the agent (the callee).
    pub fn agent(parts: Vec<Part>) -> Self {
        Self {
            kind: "message".into(),
            role: "agent".into(),
            message_id: hex::encode(rand::random::<[u8; 8]>()),
            context_id: None,
            task_id: None,
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
                Part::Text { text } => Some(text.as_str()),
                Part::Data { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The first data part.
    pub fn data(&self) -> Option<&Value> {
        self.parts.iter().find_map(|p| match p {
            Part::Data { data } => Some(data),
            Part::Text { .. } => None,
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
}

/// `message/send` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SendParams {
    /// The message.
    pub message: Message,
}

/// `tasks/get` and `tasks/cancel` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskIdParams {
    /// The task id.
    pub id: String,
}

/// A task status.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Status {
    /// The A2A state (`working`, `input-required`, …).
    pub state: String,
    /// The answer, the reason, or the question.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<Message>,
    /// When it changed (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

/// An A2A task.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    /// Always `task`.
    pub kind: String,
    /// Task id.
    pub id: String,
    /// The conversation.
    pub context_id: String,
    /// Where it stands.
    pub status: Status,
}

impl Task {
    /// The task's state in okbot's terms; an unknown state counts as failed.
    pub fn state(&self) -> PeerTaskState {
        self.status.state.parse().unwrap_or(PeerTaskState::Failed)
    }
}

/// The Agent Card of a member, for its team.
pub fn agent_card(name: &str, description: &str, url: &str, skills: &[String]) -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "name": name,
        "description": description,
        "url": url,
        "preferredTransport": "JSONRPC",
        "version": env!("CARGO_PKG_VERSION"),
        "capabilities": {
            "streaming": false,
            "pushNotifications": false,
            "extensions": [{"uri": EXTENSION, "required": true,
                            "description": "okbot delegation context (origin, chain, deadline, budget, trace)"}],
        },
        "defaultInputModes": ["text/plain"],
        "defaultOutputModes": ["text/plain"],
        "skills": skills.iter().map(|s| json!({
            "id": s, "name": s, "description": s, "tags": [s],
        })).collect::<Vec<_>>(),
        "securitySchemes": {"okbotPeer": {"type": "http", "scheme": "OkbotPeer",
                            "description": "okp1 token signed by a team member's Ed25519 key"}},
        "security": [{"okbotPeer": []}],
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

    #[test]
    fn wire_format() {
        let message = Message::user(
            "m-1",
            "c-1",
            vec![Part::Text {
                text: "Find the release notes".into(),
            }],
        )
        .with_context(&context());
        let mut request = Request::new("message/send", json!({"message": message}));
        request.id = json!(1);
        insta::with_settings!({sort_maps => true}, {
            insta::assert_json_snapshot!("message_send", request);
        });
        let parsed: SendParams = serde_json::from_value(request.params).unwrap();
        assert_eq!(parsed.message.context(), Some(context()));
        assert_eq!(parsed.message.text(), "Find the release notes");

        let task = Task {
            kind: "task".into(),
            id: "t-1".into(),
            context_id: "c-1".into(),
            status: Status {
                state: "input-required".into(),
                message: Some(Message {
                    message_id: "a-1".into(),
                    ..Message::agent(vec![
                        Part::Text {
                            text: "Restart nginx?".into(),
                        },
                        Part::Data {
                            data: json!({"prompt": "p-1", "buttons": [{"id": "allow", "label": "Allow"}]}),
                        },
                    ])
                }),
                timestamp: Some("2026-10-24T09:00:00Z".into()),
            },
        };
        insta::with_settings!({sort_maps => true}, {
            insta::assert_json_snapshot!("task", task);
        });
        assert_eq!(task.state(), PeerTaskState::InputRequired);
        let mut card = agent_card(
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
