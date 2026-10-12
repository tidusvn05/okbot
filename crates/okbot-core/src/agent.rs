//! The agent port.

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::async_trait;
use crate::caps::{AgentCaps, ToolPolicy};
use crate::secret::Secret;

/// An MCP server the agent may call during a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServer {
    /// Server name as the agent sees it (`kb`, `bot`).
    pub name: String,
    /// Streamable HTTP endpoint.
    pub url: String,
    /// Name of the environment variable holding the bearer token. The token itself is in
    /// [`AgentTurn::env`], never in a command line or config file.
    pub bearer_token_env: String,
}

/// Progress while an agent works.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AgentProgress {
    /// A piece of the answer.
    TextDelta(String),
    /// A whole message (backends that do not stream pieces).
    Text(String),
    /// The agent started using a tool.
    ToolUse(String),
}

/// Receives [`AgentProgress`]; called from the backend's output reader, so keep it quick.
#[derive(Clone)]
pub struct ProgressFn(pub std::sync::Arc<dyn Fn(AgentProgress) + Send + Sync>);

impl ProgressFn {
    /// Wraps a callback.
    pub fn new(f: impl Fn(AgentProgress) + Send + Sync + 'static) -> Self {
        Self(std::sync::Arc::new(f))
    }

    /// Reports progress.
    pub fn report(&self, progress: AgentProgress) {
        (self.0)(progress);
    }
}

impl std::fmt::Debug for ProgressFn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ProgressFn")
    }
}

/// A native session to continue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resume {
    /// The backend that owns the session ([`AgentBackend::name`] of the one that answered).
    pub backend: String,
    /// Session id from [`AgentReply::session_id`].
    pub session: String,
    /// The new messages only (the session already holds the earlier ones).
    pub prompt: String,
}

/// One agent turn.
#[derive(Debug, Clone)]
pub struct AgentTurn {
    /// Stable instructions: persona, rules, knowledge catalog, user profile.
    pub system: String,
    /// The conversation transcript and the new message.
    pub prompt: String,
    /// Tools the agent may use.
    pub tools: ToolPolicy,
    /// MCP servers for this run.
    pub mcp: Vec<McpServer>,
    /// Extra environment variables for the agent process (run tokens).
    pub env: Vec<(String, Secret<String>)>,
    /// Empty working directory for the agent process.
    pub workdir: PathBuf,
    /// Upper bound for the run.
    pub timeout: Duration,
    /// A session to continue instead of sending [`AgentTurn::prompt`]. Backends that cannot
    /// resume it (another backend's session, expired) use the full prompt instead.
    pub resume: Option<Resume>,
    /// Progress callback.
    pub progress: Option<ProgressFn>,
}

/// Token accounting, when the backend reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    /// Input tokens.
    pub input: u64,
    /// Output tokens.
    pub output: u64,
    /// Reasoning tokens.
    pub reasoning: u64,
    /// Tokens read from the prompt cache.
    pub cache_read: u64,
    /// Tokens written to the prompt cache.
    pub cache_write: u64,
}

/// The result of a successful turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentReply {
    /// The answer in Markdown.
    pub text: String,
    /// Which backend answered (`claude`, `codex`).
    pub backend: String,
    /// Model, when known.
    pub model: Option<String>,
    /// Native session id, for resuming.
    pub session_id: Option<String>,
    /// Token usage, when reported.
    pub usage: Option<TokenUsage>,
    /// Wall-clock duration.
    pub duration: Duration,
}

/// Why a turn failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AgentError {
    /// The backend cannot run at all (CLI missing, logged out).
    #[error("agent unavailable: {0}")]
    Unavailable(String),
    /// The backend cannot satisfy the turn (for example the tool policy); try another backend.
    #[error("agent cannot handle this turn: {0}")]
    Unsupported(String),
    /// The run took longer than the turn's timeout.
    #[error("agent timed out after {0:?}")]
    Timeout(Duration),
    /// The run failed. `retryable` says whether another backend (or a later retry) may succeed.
    #[error("agent failed: {message}")]
    Failed {
        /// What went wrong, safe to log (never shown to chat users).
        message: String,
        /// Whether trying again elsewhere makes sense.
        retryable: bool,
    },
}

impl AgentError {
    /// Whether a fallback backend should be tried.
    pub fn should_fall_back(&self) -> bool {
        match self {
            Self::Unavailable(_) | Self::Unsupported(_) | Self::Timeout(_) => true,
            Self::Failed { retryable, .. } => *retryable,
        }
    }
}

/// Runs agent turns (design §4.1).
///
/// Dropping the future returned by [`AgentBackend::run`] must stop the work (and kill any child
/// process): the runtime cancels turns that way.
#[async_trait]
pub trait AgentBackend: Send + Sync {
    /// Short name for logs and audit (`claude`, `fallback(claude,codex)`).
    fn name(&self) -> &str;

    /// What this backend supports natively.
    fn caps(&self) -> AgentCaps;

    /// Runs one turn.
    async fn run(&self, turn: AgentTurn) -> Result<AgentReply, AgentError>;
}
