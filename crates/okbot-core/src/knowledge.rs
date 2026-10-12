//! The knowledge port.

use std::fmt;
use std::time::Duration;

use crate::agent::McpServer;
use crate::id::{ConversationKey, UserRef};
use crate::secret::Secret;
use crate::{BoxError, async_trait};

/// A knowledge failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum KnowledgeError {
    /// The bundle could not be opened or indexed.
    #[error("knowledge bundle: {0}")]
    Bundle(#[source] BoxError),
    /// Any other failure.
    #[error("knowledge error: {0}")]
    Other(#[source] BoxError),
}

/// Who is asking, which decides what knowledge and memory is visible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requester {
    /// The conversation.
    pub conversation: ConversationKey,
    /// The user.
    pub user: UserRef,
    /// The room may see documents marked `audience: private`.
    pub internal: bool,
}

/// Knowledge placed in the system prompt.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct KnowledgeContext {
    /// The bot's persona documents.
    pub persona: String,
    /// The bundle catalog, as visible to the requester.
    pub catalog: String,
    /// The requester's profile, if one exists.
    pub user_profile: Option<String>,
}

/// Access to knowledge tools for one agent run. Dropping it revokes the access.
pub struct RunGrant {
    /// MCP servers to give the agent.
    pub mcp: Vec<McpServer>,
    /// Environment variables carrying the run token.
    pub env: Vec<(String, Secret<String>)>,
    _revoke_on_drop: Box<dyn Send + Sync>,
}

impl RunGrant {
    /// Creates a grant; `guard` is dropped (and should revoke the token) when the grant is dropped.
    pub fn new(
        mcp: Vec<McpServer>,
        env: Vec<(String, Secret<String>)>,
        guard: impl Send + Sync + 'static,
    ) -> Self {
        Self {
            mcp,
            env,
            _revoke_on_drop: Box::new(guard),
        }
    }

    /// A grant without tools, for backends that cannot use MCP.
    pub fn none() -> Self {
        Self::new(Vec::new(), Vec::new(), ())
    }
}

impl fmt::Debug for RunGrant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunGrant")
            .field("mcp", &self.mcp)
            .field("env", &self.env)
            .finish_non_exhaustive()
    }
}

/// The bot's knowledge and memory (design §5).
#[async_trait]
pub trait KnowledgeProvider: Send + Sync {
    /// Persona, catalog and profile for the system prompt.
    async fn context(&self, who: &Requester) -> Result<KnowledgeContext, KnowledgeError>;

    /// Issues tool access for one run, scoped to `who`, valid for at most `ttl`.
    async fn grant(&self, who: &Requester, ttl: Duration) -> Result<RunGrant, KnowledgeError>;

    /// Text search for backends without MCP: the most relevant excerpts for `query`, or an empty
    /// string when nothing matches.
    async fn excerpts(&self, who: &Requester, query: &str) -> Result<String, KnowledgeError>;
}
