//! Slack channel for okbot (Socket Mode).
//!
//! Socket Mode needs no public URL: okbot opens a WebSocket with the app-level token
//! (`xapp-…`), acknowledges every envelope at once (Slack's 3-second rule) and answers with the
//! Web API using the bot token (`xoxb-…`). The app subscribes to `app_mention` and `message.im`;
//! replies go into the message's thread.

mod event;
mod web;

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use futures::{SinkExt as _, StreamExt as _};
use okbot_core::{
    CancellationToken, Channel, ChannelCaps, ChannelError, ChannelId, InboundSink, LengthUnit,
    MarkdownDialect, MessageId, RenderedMessage, ReplyTarget, Secret, SentRef, async_trait,
};
use serde_json::{Value, json};
use tokio_websockets::{ClientBuilder, Message};

pub use web::{SLACK_API, SlackError, SlackWeb};

/// Settings of one Slack app.
#[derive(Debug, Clone)]
pub struct SlackConfig {
    /// Channel id, `slack:<instance>`.
    pub id: ChannelId,
    /// App-level token with `connections:write` (`xapp-…`).
    pub app_token: Secret<String>,
    /// Bot token (`xoxb-…`).
    pub bot_token: Secret<String>,
    /// Web API base (tests point it at a fake).
    pub api_base: String,
}

/// A Slack app as an okbot [`Channel`].
pub struct SlackChannel {
    id: ChannelId,
    web: SlackWeb,
    caps: ChannelCaps,
    me: OnceLock<String>,
}

impl std::fmt::Debug for SlackChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SlackChannel")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

fn channel_error(error: SlackError) -> ChannelError {
    match error {
        SlackError::Api(code)
            if matches!(
                code.as_str(),
                "invalid_auth" | "not_authed" | "account_inactive" | "token_revoked"
            ) =>
        {
            ChannelError::Auth(code)
        }
        SlackError::Api(code) if code == "ratelimited" => {
            ChannelError::RateLimited { retry_after: None }
        }
        e @ SlackError::Transport(_) => ChannelError::Transient(Box::new(e)),
        e => ChannelError::Failed(Box::new(e)),
    }
}

/// What the socket loop should do next.
enum Next {
    Reconnect,
    Stop,
}

impl SlackChannel {
    /// Creates the channel. Nothing is contacted until [`Channel::run`].
    pub fn new(config: SlackConfig) -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        Self {
            id: config.id,
            web: SlackWeb::new(&config.api_base, config.app_token, config.bot_token),
            caps: ChannelCaps {
                // Slack accepts 40,000 characters but recommends far less per message.
                max_message_len: 3000,
                length_unit: LengthUnit::Chars,
                dialect: MarkdownDialect::SlackMrkdwn,
                threads: true,
                edit: true,
                typing: false,
            },
            me: OnceLock::new(),
        }
    }

    /// The Web API (for `okbot doctor`).
    pub fn web(&self) -> &SlackWeb {
        &self.web
    }

    async fn session(
        &self,
        sink: &Arc<dyn InboundSink>,
        shutdown: &CancellationToken,
        me: &str,
    ) -> Result<Next, ChannelError> {
        let url = self.web.connections_open().await.map_err(channel_error)?;
        let builder = ClientBuilder::new()
            .uri(&url)
            .map_err(|e| ChannelError::Failed(Box::new(e)))?;
        let (mut socket, _) = builder
            .connect()
            .await
            .map_err(|e| ChannelError::Transient(Box::new(e)))?;
        loop {
            let frame = tokio::select! {
                () = shutdown.cancelled() => {
                    let _ = socket.close().await;
                    return Ok(Next::Stop);
                }
                frame = socket.next() => frame,
            };
            let Some(frame) = frame else {
                return Ok(Next::Reconnect);
            };
            let frame = frame.map_err(|e| ChannelError::Transient(Box::new(e)))?;
            if frame.is_close() {
                return Ok(Next::Reconnect);
            }
            let Some(text) = frame.as_text() else {
                continue;
            };
            let Ok(envelope) = serde_json::from_str::<Value>(text) else {
                continue;
            };
            // Acknowledge first: Slack retries envelopes not acknowledged within 3 seconds.
            if let Some(id) = envelope["envelope_id"].as_str() {
                let ack = json!({ "envelope_id": id }).to_string();
                socket
                    .send(Message::text(ack))
                    .await
                    .map_err(|e| ChannelError::Transient(Box::new(e)))?;
            }
            match envelope["type"].as_str() {
                Some("hello") => tracing::info!(channel = %self.id, "Slack socket connected"),
                Some("disconnect") => return Ok(Next::Reconnect),
                Some("events_api") => {
                    if let Some(event) = event::to_inbound(&self.id, &envelope["payload"], me)
                        && sink.deliver(event).await.is_err()
                    {
                        return Ok(Next::Stop);
                    }
                }
                _ => {}
            }
        }
    }
}

#[async_trait]
impl Channel for SlackChannel {
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
        let me = self.web.auth_test().await.map_err(channel_error)?;
        let me = self.me.get_or_init(|| me).clone();
        let mut failures: u32 = 0;
        loop {
            match self.session(&sink, &shutdown, &me).await {
                Ok(Next::Stop) => return Ok(()),
                Ok(Next::Reconnect) => {
                    failures = 0;
                    tracing::info!(channel = %self.id, "Slack asked to reconnect");
                }
                Err(error @ ChannelError::Auth(_)) => return Err(error),
                Err(error) => {
                    let wait = Duration::from_secs(1u64 << failures.min(5));
                    failures = failures.saturating_add(1);
                    tracing::warn!(channel = %self.id, %error, ?wait, "Slack socket failed");
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
        let ts = self
            .web
            .post_message(
                &to.conversation.room,
                to.conversation.thread.as_deref(),
                &message.text,
            )
            .await
            .map_err(channel_error)?;
        Ok(SentRef {
            target: to.clone(),
            message_id: MessageId::new(ts),
        })
    }

    async fn edit(&self, sent: &SentRef, message: &RenderedMessage) -> Result<(), ChannelError> {
        self.web
            .update(
                &sent.target.conversation.room,
                sent.message_id.as_str(),
                &message.text,
            )
            .await
            .map_err(channel_error)
    }
}

#[cfg(test)]
mod tests;
