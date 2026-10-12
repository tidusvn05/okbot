//! The chat platform port.

use std::sync::Arc;
use std::time::Duration;

use crate::caps::ChannelCaps;
use crate::id::ChannelId;
use crate::message::{InboundEvent, RenderedMessage, ReplyTarget, SentRef};
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

/// The runtime stopped accepting events (it is shutting down).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the runtime is no longer accepting events")]
pub struct SinkClosed;

/// Receives normalized events from channels. Implemented by the runtime.
#[async_trait]
pub trait InboundSink: Send + Sync {
    /// Hands one event to the runtime. Returns quickly; processing happens elsewhere.
    async fn deliver(&self, event: InboundEvent) -> Result<(), SinkClosed>;
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

    /// The webhook endpoint, for platforms that push events over HTTP.
    fn webhook(&self) -> Option<Arc<dyn WebhookHandler>> {
        None
    }
}
