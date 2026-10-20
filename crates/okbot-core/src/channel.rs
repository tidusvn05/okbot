//! The chat platform port.

use std::sync::Arc;
use std::time::Duration;

use crate::caps::ChannelCaps;
use crate::id::{ChannelId, ConversationKey, UserId};
use crate::message::{InboundEvent, RenderedMessage, ReplyTarget, SentRef};
use crate::prompt::{Prompt, PromptAnswer};
use crate::webhook::WebhookHandler;
use crate::{BoxError, CancellationToken, async_trait};

/// An error from a channel adapter.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ChannelError {
    /// The platform does not support the operation (see [`ChannelCaps`]).
    #[error("not supported by this platform: {0}")]
    Unsupported(&'static str),
    /// Credentials were rejected. The channel stops; the operator must fix the configuration.
    #[error("authentication failed: {0}")]
    Auth(String),
    /// The platform asked to slow down.
    #[error("rate limited")]
    RateLimited {
        /// How long to wait, when the platform says.
        retry_after: Option<Duration>,
    },
    /// A temporary failure; retrying later may succeed.
    #[error("temporary failure: {0}")]
    Transient(#[source] BoxError),
    /// A permanent failure for this request.
    #[error("{0}")]
    Failed(#[source] BoxError),
}

impl ChannelError {
    /// Whether retrying the same request later may succeed.
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::RateLimited { .. } | Self::Transient(_))
    }
}

/// A command for the platform's command menu (`/` on Discord and Telegram).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandInfo {
    /// Without `/`: a-z, 0-9 and `_`, at most 32.
    pub name: String,
    /// One line, at most 100 characters.
    pub description: String,
    /// The description in other languages: (language code such as `en`, `ja`, text).
    pub descriptions: Vec<(String, String)>,
    /// Positional parameters, required ones first.
    pub params: Vec<CommandParam>,
}

/// A parameter of a [`CommandInfo`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandParam {
    /// Its name.
    pub name: String,
    /// Must be given.
    pub required: bool,
    /// May contain spaces (the rest of the line).
    pub rest: bool,
}

/// The runtime stopped accepting events (it is shutting down).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the runtime is no longer accepting events")]
pub struct SinkClosed;

/// Receives normalized events from channels. Implemented by the runtime.
#[async_trait]
pub trait InboundSink: Send + Sync {
    /// Hands one event to the runtime. Returns quickly; processing happens elsewhere.
    async fn deliver(&self, event: InboundEvent) -> Result<(), SinkClosed>;

    /// Hands over the answer to a [`Prompt`]. Returns quickly. Sinks without prompts ignore it.
    async fn answer(&self, _answer: PromptAnswer) -> Result<(), SinkClosed> {
        Ok(())
    }
}

/// A chat platform adapter (design §3.2).
///
/// A channel receives events (through [`Channel::run`] or a [`WebhookHandler`]) and sends replies.
/// It knows nothing about agents or knowledge.
#[async_trait]
pub trait Channel: Send + Sync {
    /// The configured instance, for example `zalo:shop`.
    fn id(&self) -> &ChannelId;

    /// What the platform supports.
    fn caps(&self) -> &ChannelCaps;

    /// Receives events until `shutdown` is cancelled, delivering them to `sink`.
    ///
    /// Polling and socket platforms loop here and reconnect on their own. Webhook platforms may
    /// only do start-up work (such as registering the webhook) and then wait for `shutdown`.
    /// Returning an error stops this channel only.
    async fn run(
        &self,
        sink: Arc<dyn InboundSink>,
        shutdown: CancellationToken,
    ) -> Result<(), ChannelError>;

    /// Sends one message.
    async fn send(
        &self,
        to: &ReplyTarget,
        message: &RenderedMessage,
    ) -> Result<SentRef, ChannelError>;

    /// Replaces the text of a sent message. Only called when [`ChannelCaps::edit`] is true.
    async fn edit(&self, _sent: &SentRef, _message: &RenderedMessage) -> Result<(), ChannelError> {
        Err(ChannelError::Unsupported("edit"))
    }

    /// Shows a typing indicator once. The runtime repeats it while an agent works.
    async fn typing(&self, _to: &ReplyTarget) -> Result<(), ChannelError> {
        Ok(())
    }

    /// Shows a prompt (a message with buttons) in a conversation. Answers come back through
    /// [`InboundSink::answer`]. Platforms without buttons return [`ChannelError::Unsupported`].
    async fn prompt(&self, _to: &ReplyTarget, _prompt: &Prompt) -> Result<SentRef, ChannelError> {
        Err(ChannelError::Unsupported("prompt"))
    }

    /// Replaces a prompt's text and removes its buttons (answered, cancelled or expired). With
    /// `keep_buttons`, only the text changes and the prompt can be answered again.
    async fn close_prompt(
        &self,
        _sent: &SentRef,
        _message: &RenderedMessage,
        _keep_buttons: bool,
    ) -> Result<(), ChannelError> {
        Err(ChannelError::Unsupported("prompt"))
    }

    /// The direct chat with `user`, opened if needed, for messages the bot starts (notifications)
    /// to someone who has not written to it yet. Platforms whose bots cannot start a direct chat,
    /// or need an extra permission for it, return [`ChannelError::Unsupported`]; the runtime then
    /// only uses direct chats it has already seen.
    async fn open_direct(&self, _user: &UserId) -> Result<ConversationKey, ChannelError> {
        Err(ChannelError::Unsupported("open_direct"))
    }

    /// Lists the bot's commands in the platform's command menu. Called once, before
    /// [`Channel::run`], and only when the bot has commands of its own. Commands always arrive as
    /// text messages starting with `/name`: platforms whose menu sends something else (Discord's
    /// slash commands) turn it into such a message. Platforms without a menu ignore this.
    async fn set_commands(&self, _commands: &[CommandInfo]) -> Result<(), ChannelError> {
        Ok(())
    }

    /// The webhook endpoint, for platforms that push events over HTTP.
    fn webhook(&self) -> Option<Arc<dyn WebhookHandler>> {
        None
    }
}
