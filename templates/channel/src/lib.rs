//! Example channel for okbot: the starting point for a new platform.
//!
//! A channel does three things (`docs/extending/channel.md`):
//!
//! 1. [`Channel::run`] receives platform events, turns each into an [`InboundEvent`] and hands it
//!    to the sink. It reconnects on transient errors and returns when `shutdown` fires.
//! 2. [`Channel::send`] (and optionally `edit`, `typing`) posts already-rendered text.
//! 3. [`Channel::caps`] tells the runtime how long a message may be and which Markdown dialect the
//!    platform speaks; the runtime renders and splits replies for you.
//!
//! Everything else (access rules, deduplication, history, agents, knowledge) is the runtime's job.

mod client;

use std::sync::Arc;
use std::time::Duration;

use okbot_core::{
    CancellationToken, Channel, ChannelCaps, ChannelError, ChannelId, ConversationKey,
    InboundEvent, InboundSink, MessageId, RenderedMessage, ReplyTarget, SentRef, UserId, UserRef,
    async_trait,
};

pub use client::{Client, ClientError, PlatformMessage};

/// An example platform as an okbot [`Channel`].
#[derive(Debug)]
pub struct ExampleChannel {
    id: ChannelId,
    client: Arc<Client>,
    caps: ChannelCaps,
}

impl ExampleChannel {
    /// Creates the channel. Contact the platform in [`Channel::run`], not here.
    pub fn new(id: ChannelId, client: Arc<Client>) -> Self {
        Self {
            id,
            client,
            // TODO(platform): the real limits and dialect.
            caps: ChannelCaps::basic(),
        }
    }

    fn to_inbound(&self, m: PlatformMessage) -> InboundEvent {
        InboundEvent {
            event_id: m.id.clone(),
            conversation: ConversationKey {
                channel: self.id.clone(),
                space: None,
                room: m.room,
                thread: None,
            },
            author: UserRef {
                id: UserId::new(m.user),
                display_name: m.user_name,
                is_bot: false,
            },
            text: m.text,
            attachments: Vec::new(),
            // TODO(platform): detect mentions of the bot in group rooms.
            mentions_bot: false,
            is_direct: m.direct,
            restricted: false,
            message_id: Some(MessageId::new(m.id)),
            // TODO(platform): the platform's timestamp.
            sent_at: jiff::Timestamp::now(),
            locale: None,
        }
    }
}

fn channel_error(error: ClientError) -> ChannelError {
    match error {
        ClientError::Unauthorized => ChannelError::Auth("unauthorized".into()),
        e @ ClientError::Transport(_) => ChannelError::Transient(Box::new(e)),
    }
}

#[async_trait]
impl Channel for ExampleChannel {
    fn id(&self) -> &ChannelId {
        &self.id
    }

    fn caps(&self) -> &ChannelCaps {
        &self.caps
    }

    async fn run(
        &self,
        sink: Arc<dyn InboundSink>,
        shutdown: CancellationToken,
    ) -> Result<(), ChannelError> {
        let mut failures: u32 = 0;
        loop {
            let batch = tokio::select! {
                () = shutdown.cancelled() => return Ok(()),
                batch = self.client.next_messages() => batch,
            };
            match batch {
                Ok(messages) => {
                    failures = 0;
                    for m in messages {
                        if sink.deliver(self.to_inbound(m)).await.is_err() {
                            return Ok(()); // the runtime is shutting down
                        }
                    }
                }
                Err(ClientError::Unauthorized) => {
                    return Err(channel_error(ClientError::Unauthorized));
                }
                Err(error) => {
                    let wait = Duration::from_secs(1u64 << failures.min(5));
                    failures = failures.saturating_add(1);
                    tracing::warn!(channel = %self.id, %error, ?wait, "platform error");
                    tokio::select! {
                        () = shutdown.cancelled() => return Ok(()),
                        () = tokio::time::sleep(wait) => {}
                    }
                }
            }
        }
    }

    async fn send(
        &self,
        to: &ReplyTarget,
        message: &RenderedMessage,
    ) -> Result<SentRef, ChannelError> {
        let id = self
            .client
            .post(&to.conversation.room, &message.text)
            .await
            .map_err(channel_error)?;
        Ok(SentRef {
            target: to.clone(),
            message_id: MessageId::new(id),
        })
    }
}

#[cfg(test)]
mod tests;
