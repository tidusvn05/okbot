//! The conversation store port.

use std::time::Duration;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::agent::TokenUsage;
use crate::caps::ToolPolicy;
use crate::id::{ChannelId, ConversationKey, MessageId, RunId, UserId};
use crate::peer::PeerTaskRecord;
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
    /// Who the run was for ([`person_key`]), counted against `[limits]`; `None` for runs nobody
    /// asked for in chat.
    pub person: Option<String>,
}

/// The key a person's usage and audit entries are stored under: `person:<id>` for linked
/// accounts (`[people]`, or the owner of a personal bot), else the account
/// `platform:instance/user-id`.
pub fn person_key(channel: &ChannelId, user: &UserId, person: Option<&str>) -> String {
    match person {
        Some(id) => format!("person:{id}"),
        None => format!("{channel}/{user}"),
    }
}

/// What one person used over a period.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Usage {
    /// Agent turns.
    pub turns: u64,
    /// Input plus output tokens, as reported.
    pub tokens: u64,
}

/// What happened to a tool call (ADR 0011).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ToolDecision {
    /// Sent to the server.
    Allowed,
    /// Refused by the rules (role, room, tool lists).
    Denied,
    /// A person declined it (or did not answer) when asked to confirm.
    Declined,
}

impl ToolDecision {
    /// The name stored and shown.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allowed => "allowed",
            Self::Denied => "denied",
            Self::Declined => "declined",
        }
    }

    /// Parses [`ToolDecision::as_str`].
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "allowed" => Some(Self::Allowed),
            "denied" => Some(Self::Denied),
            "declined" => Some(Self::Declined),
            _ => None,
        }
    }
}

/// One call of an `[[mcp]]` tool, by an agent or a chat command. Arguments and results are not
/// kept: they may hold secrets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallRecord {
    /// The agent run, when an agent made the call.
    pub run_id: Option<RunId>,
    /// The `[[command]]`, when a person ran one.
    pub command: Option<String>,
    /// Who it was for ([`person_key`]).
    pub person: String,
    /// Where.
    pub conversation: ConversationKey,
    /// The `[[mcp]]` server.
    pub server: String,
    /// The tool.
    pub tool: String,
    /// What happened.
    pub decision: ToolDecision,
    /// The transport error or the tool's own error flag, when the call failed.
    pub error: Option<String>,
    /// Time from the request to the answer (including a confirmation).
    pub duration: Duration,
    /// When the call was made.
    pub at: Timestamp,
}

/// A role given to an account for a limited time (`okbot role grant`, `/grant`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleGrant {
    /// A role from `[roles]`.
    pub role: String,
    /// The account, as `platform:instance/user-id`.
    pub account: String,
    /// Who granted it (an account, or `operator` for the terminal).
    pub granted_by: String,
    /// When it was granted.
    pub granted_at: Timestamp,
    /// When it ends.
    pub expires_at: Timestamp,
}

/// Where one firing of a task is (design §5.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FireState {
    /// Claimed; being delivered or waiting for a retry.
    Sending,
    /// Delivered.
    Sent,
    /// Gave up after retries.
    Failed,
    /// Too late to deliver (the bot was down).
    Missed,
}

impl FireState {
    /// The name stored and shown.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sending => "sending",
            Self::Sent => "sent",
            Self::Failed => "failed",
            Self::Missed => "missed",
        }
    }

    /// Parses [`FireState::as_str`].
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "sending" => Some(Self::Sending),
            "sent" => Some(Self::Sent),
            "failed" => Some(Self::Failed),
            "missed" => Some(Self::Missed),
            _ => None,
        }
    }

    /// No more delivery attempts follow.
    pub fn is_final(self) -> bool {
        !matches!(self, Self::Sending)
    }
}

/// One firing of a task: the task id and its planned time identify it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fire {
    /// Task id (`tasks/people/owner/t-7k2m9q`).
    pub task_id: String,
    /// The planned time.
    pub fire_at: Timestamp,
    /// Where it is.
    pub state: FireState,
    /// Delivery attempts so far.
    pub attempts: u32,
    /// Last change.
    pub updated_at: Timestamp,
}

/// A sealed secret's identity, without the secret (for listings).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedEntry {
    /// What it is (`oauth-token`, `oauth-client`).
    pub kind: String,
    /// The `[[mcp]]` server.
    pub server: String,
    /// Whose it is: a [`person_key`], `shared`, or the issuer of a client registration.
    pub subject: String,
    /// Last change.
    pub updated_at: Timestamp,
}

/// One run of an automation (ADR 0013).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomationRun {
    /// The `[[automation]]` name.
    pub automation: String,
    /// When it ran.
    pub at: Timestamp,
    /// New items acted on (0 when nothing was new).
    pub items: u32,
    /// What it did: `text`, `prompt`, `none` (nothing new) or `primed` (first poll).
    pub action: String,
    /// Why it failed, when it did.
    pub error: Option<String>,
}

fn unsupported() -> StoreError {
    StoreError::Backend("not supported by this store".into())
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

    /// Claims a task firing: records it as [`FireState::Sending`] the first time and returns its
    /// current record, so a firing that was already sent is never sent again.
    async fn claim_fire(&self, task_id: &str, fire_at: Timestamp) -> Result<Fire, StoreError>;

    /// Updates a claimed firing.
    async fn set_fire(
        &self,
        task_id: &str,
        fire_at: Timestamp,
        state: FireState,
        attempts: u32,
    ) -> Result<(), StoreError>;

    /// The newest `limit` firings, newest first (for `okbot doctor` and `okbot task`).
    async fn recent_fires(&self, limit: usize) -> Result<Vec<Fire>, StoreError>;

    /// Remembers the direct conversation of `user` on `channel` (reminders sent there).
    async fn set_direct_chat(
        &self,
        channel: &ChannelId,
        user: &UserId,
        conversation: &ConversationKey,
    ) -> Result<(), StoreError>;

    /// The latest direct conversation of `user` on `channel`.
    async fn direct_chat(
        &self,
        channel: &ChannelId,
        user: &UserId,
    ) -> Result<Option<ConversationKey>, StoreError>;

    /// Turns and tokens of `person` ([`person_key`]) in runs started at or after `since`.
    async fn usage_since(&self, person: &str, since: Timestamp) -> Result<Usage, StoreError>;

    /// Records a tool call.
    async fn record_tool_call(&self, call: ToolCallRecord) -> Result<(), StoreError>;

    /// Tool calls made at or after `since`, newest first, at most `limit`.
    async fn tool_calls(
        &self,
        since: Timestamp,
        limit: usize,
    ) -> Result<Vec<ToolCallRecord>, StoreError>;

    /// Grants a role until `grant.expires_at`, replacing an earlier grant of the same role to the
    /// same account.
    async fn grant_role(&self, grant: RoleGrant) -> Result<(), StoreError>;

    /// Ends a grant before it expires. Returns whether there was one.
    async fn revoke_role(&self, role: &str, account: &str) -> Result<bool, StoreError>;

    /// Grants that have not expired at `now`.
    async fn role_grants(&self, now: Timestamp) -> Result<Vec<RoleGrant>, StoreError>;

    /// Deletes messages, runs, tool calls, expired grants, dedupe marks, finished firings and
    /// automation records older than `before`. Returns rows removed.
    async fn purge_before(&self, before: Timestamp) -> Result<u64, StoreError>;

    /// A sealed secret (ADR 0012): the runtime seals and opens it; the store only keeps bytes.
    async fn sealed(
        &self,
        kind: &str,
        server: &str,
        subject: &str,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        let _ = (kind, server, subject);
        Err(unsupported())
    }

    /// Stores or replaces a sealed secret.
    async fn set_sealed(
        &self,
        kind: &str,
        server: &str,
        subject: &str,
        sealed: Vec<u8>,
    ) -> Result<(), StoreError> {
        let _ = (kind, server, subject, sealed);
        Err(unsupported())
    }

    /// Deletes a sealed secret. Returns whether there was one.
    async fn delete_sealed(
        &self,
        kind: &str,
        server: &str,
        subject: &str,
    ) -> Result<bool, StoreError> {
        let _ = (kind, server, subject);
        Err(unsupported())
    }

    /// The sealed secrets of `kind`, without their values, by server and subject.
    async fn sealed_entries(&self, kind: &str) -> Result<Vec<SealedEntry>, StoreError> {
        let _ = kind;
        Err(unsupported())
    }

    /// Records `items` as seen by `automation` and returns those that were not seen before, in
    /// their order.
    async fn claim_items(
        &self,
        automation: &str,
        items: Vec<String>,
    ) -> Result<Vec<String>, StoreError> {
        let _ = (automation, items);
        Err(unsupported())
    }

    /// Forgets that `automation` saw `items`, so they count as new again (items not acted on).
    async fn release_items(&self, automation: &str, items: Vec<String>) -> Result<(), StoreError> {
        let _ = (automation, items);
        Err(unsupported())
    }

    /// Records a run of an automation.
    async fn record_automation_run(&self, run: AutomationRun) -> Result<(), StoreError> {
        let _ = run;
        Err(unsupported())
    }

    /// Runs at or after `since`, of one automation or of all, newest first, at most `limit`.
    async fn automation_runs(
        &self,
        automation: Option<&str>,
        since: Timestamp,
        limit: usize,
    ) -> Result<Vec<AutomationRun>, StoreError> {
        let _ = (automation, since, limit);
        Err(unsupported())
    }

    /// Stores a peer task (ADR 0014), replacing the one with the same id and direction.
    async fn put_peer_task(&self, task: PeerTaskRecord) -> Result<(), StoreError> {
        let _ = task;
        Err(unsupported())
    }

    /// A peer task by its id and direction.
    async fn peer_task(
        &self,
        inbound: bool,
        id: &str,
    ) -> Result<Option<PeerTaskRecord>, StoreError> {
        let _ = (inbound, id);
        Err(unsupported())
    }

    /// The task a message created, if any (a retried request returns the same task).
    async fn peer_task_by_message(
        &self,
        inbound: bool,
        team: &str,
        peer: &str,
        message_id: &str,
    ) -> Result<Option<PeerTaskRecord>, StoreError> {
        let _ = (inbound, team, peer, message_id);
        Err(unsupported())
    }

    /// Peer tasks, newest first, at most `limit`.
    async fn peer_tasks(&self, limit: usize) -> Result<Vec<PeerTaskRecord>, StoreError> {
        let _ = limit;
        Err(unsupported())
    }

    /// Marks inbound tasks that were not finished as failed ("interrupted"), at start-up: work with
    /// side effects is never re-run by itself. Returns how many.
    async fn interrupt_peer_tasks(&self, now: Timestamp) -> Result<u64, StoreError> {
        let _ = now;
        Err(unsupported())
    }
}
