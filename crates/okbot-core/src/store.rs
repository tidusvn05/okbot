//! The conversation store port.

use std::time::Duration;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::agent::TokenUsage;
use crate::caps::ToolPolicy;
use crate::id::{ChannelId, ConversationKey, MessageId, RunId, UserId};
use crate::{BoxError, async_trait};

/// A storage failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StoreError {
    /// The database was written by a newer okbot.
    #[error("the database schema version {found} is newer than this okbot supports ({supported})")]
    TooNew {
        /// Version in the database.
        found: u32,
        /// Newest version this build knows.
        supported: u32,
    },
    /// Any other failure.
    #[error("storage error: {0}")]
    Backend(#[source] BoxError),
}

/// Wrong pairing codes tolerated before pending codes are cancelled.
pub const PAIRING_MAX_FAILURES: u32 = 5;

/// Who wrote a stored message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MessageRole {
    /// A chat user.
    User,
    /// The bot.
    Bot,
}

/// A message to store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewMessage {
    /// Conversation.
    pub conversation: ConversationKey,
    /// Author role.
    pub role: MessageRole,
    /// Author id (the user, or the bot's own id when known).
    pub author_id: Option<UserId>,
    /// Author display name.
    pub author_name: String,
    /// Text (canonical Markdown for bot messages).
    pub text: String,
    /// Platform message id.
    pub message_id: Option<MessageId>,
    /// When it was sent.
    pub at: Timestamp,
}

/// A stored message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredMessage {
    /// Author role.
    pub role: MessageRole,
    /// Author display name.
    pub author_name: String,
    /// Text.
    pub text: String,
    /// When it was sent.
    pub at: Timestamp,
}

/// One agent run, for the audit table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRunRecord {
    /// Run id.
    pub run_id: RunId,
    /// Conversation.
    pub conversation: ConversationKey,
    /// Backend that produced the result (or the last one tried).
    pub backend: String,
    /// Model, when known.
    pub model: Option<String>,
    /// Tool policy of the run.
    pub tools: ToolPolicy,
    /// Token usage, when reported.
    pub usage: Option<TokenUsage>,
    /// Duration.
    pub duration: Duration,
    /// Error text, when the run failed.
    pub error: Option<String>,
    /// When the run started.
    pub started_at: Timestamp,
}

/// Stores conversations, deduplication marks and the audit trail (design §6.1).
#[async_trait]
pub trait ConversationStore: Send + Sync {
    /// Records an inbound event id. Returns `false` when it was already seen within `ttl`.
    async fn mark_seen(
        &self,
        channel: &ChannelId,
        event_id: &str,
        ttl: Duration,
    ) -> Result<bool, StoreError>;

    /// Appends a message to a conversation.
    async fn append(&self, message: NewMessage) -> Result<(), StoreError>;

    /// The newest `limit` messages after the last reset, oldest first.
    async fn recent(
        &self,
        conversation: &ConversationKey,
        limit: usize,
    ) -> Result<Vec<StoredMessage>, StoreError>;

    /// Starts a fresh history for the conversation (`/reset`). Old messages stay for audit
    /// until retention removes them.
    async fn reset(&self, conversation: &ConversationKey) -> Result<(), StoreError>;

    /// The native agent session of a conversation: `(backend, session id)`.
    async fn session(
        &self,
        conversation: &ConversationKey,
    ) -> Result<Option<(String, String)>, StoreError>;

    /// Remembers the native agent session of a conversation (cleared by [`ConversationStore::reset`]).
    async fn set_session(
        &self,
        conversation: &ConversationKey,
        backend: &str,
        session: &str,
    ) -> Result<(), StoreError>;

    /// Records an agent run.
    async fn record_run(&self, run: AgentRunRecord) -> Result<(), StoreError>;

    /// Accounts of the owner of a personal bot (paired with [`ConversationStore::redeem_pairing_code`]).
    async fn owners(&self) -> Result<Vec<(ChannelId, UserId)>, StoreError>;

    /// Removes an owner account. Returns whether it was one.
    async fn remove_owner(&self, channel: &ChannelId, user: &UserId) -> Result<bool, StoreError>;

    /// Stores a one-time pairing code, valid for `ttl`.
    async fn create_pairing_code(&self, code: &str, ttl: Duration) -> Result<(), StoreError>;

    /// Consumes `code` and makes `(channel, user)` an owner account when the code is valid.
    ///
    /// A wrong code counts as a failed attempt against every pending code; after
    /// [`PAIRING_MAX_FAILURES`] failures the pending codes are cancelled, so codes cannot be
    /// guessed.
    async fn redeem_pairing_code(
        &self,
        code: &str,
        channel: &ChannelId,
        user: &UserId,
    ) -> Result<bool, StoreError>;

    /// Deletes messages, runs and dedupe marks older than `before`. Returns rows removed.
    async fn purge_before(&self, before: Timestamp) -> Result<u64, StoreError>;
}
