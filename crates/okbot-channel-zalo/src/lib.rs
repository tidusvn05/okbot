//! Zalo Bot Platform channel for okbot (long polling and webhook).
//!
//! [Zalo Bot Platform](https://bot.zapps.me/docs/) runs an automated account inside the Zalo app.
//! This channel receives messages either by long polling (`getUpdates`, no public URL needed) or
//! by webhook (`setWebhook`, a public HTTPS URL), and replies with `sendMessage`.
//!
//! Notes from the platform (design §3.3.2):
//! - the API client is the [`zalo_bot`] crate; the token is part of every API URL, which that crate
//!   keeps out of `Debug` output and errors;
//! - messages hold at most 2000 characters, support a small Markdown subset, cannot be edited;
//! - group messages are delivered only when they mention or reply to the bot;
//! - `message.unsupported.received` replaces messages from protected users and is answered with a
//!   fixed reply, never processed.

mod event;

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use futures::StreamExt as _;
use okbot_core::{
    CancellationToken, Channel, ChannelCaps, ChannelError, ChannelId, InboundSink, LengthUnit,
    MarkdownDialect, MessageId, RenderedMessage, ReplyTarget, Secret, SentRef, WebhookHandler,
    WebhookRequest, WebhookResponse, async_trait,
};
use zalo_bot::{Bot, ChatAction, PollOptions, Token, Update};

/// The production API base.
pub const DEFAULT_API_BASE: &str = zalo_bot::API_BASE;

/// The Zalo Bot API client okbot uses (re-exported for `okbot doctor` and `okbot init`).
pub use zalo_bot;

/// A Zalo Bot API client for `token` against `base`.
///
/// okbot links rustls with ring only, so the ring provider is installed first (a no-op when it is
/// already installed).
pub fn client(base: &str, token: &Secret<String>) -> Bot {
    let _ = rustls::crypto::ring::default_provider().install_default();
    Bot::new(Token::new(token.expose().as_str())).with_base_url(base)
}

/// How the channel receives events. The two modes are mutually exclusive on Zalo's side.
#[derive(Debug, Clone)]
pub enum Delivery {
    /// `getUpdates` long polling. Works behind NAT; Zalo recommends webhooks for production.
    Polling {
        /// Long-poll timeout per request.
        timeout: Duration,
    },
    /// Zalo `POST`s events to `public_url`, authenticated by `secret`.
    Webhook {
        /// The HTTPS URL Zalo calls (it reaches okbot's HTTP server through a reverse proxy).
        public_url: String,
        /// Sent back by Zalo in `X-Bot-Api-Secret-Token` (8–256 characters).
        secret: Secret<String>,
    },
}

/// Settings of one Zalo bot.
#[derive(Debug, Clone)]
pub struct ZaloConfig {
    /// Channel id, `zalo:<instance>`.
    pub id: ChannelId,
    /// Bot token from Zalo Bot Creator.
    pub token: Secret<String>,
    /// Receiving mode.
    pub delivery: Delivery,
    /// API base URL (tests point it at a fake server).
    pub api_base: String,
}

impl ZaloConfig {
    /// Polling with a 30-second long-poll against the production API.
    pub fn polling(id: ChannelId, token: Secret<String>) -> Self {
        Self {
            id,
            token,
            delivery: Delivery::Polling {
                timeout: Duration::from_secs(30),
            },
            api_base: DEFAULT_API_BASE.to_string(),
        }
    }
}

/// State shared by the channel and its webhook handler.
struct Shared {
    id: ChannelId,
    bot: Bot,
    sink: OnceLock<Arc<dyn InboundSink>>,
}

impl Shared {
    async fn deliver(&self, update: &Update) {
        let Some(event) = event::to_inbound(&self.id, update, jiff::Timestamp::now()) else {
            tracing::debug!(channel = %self.id, event = %update.event_name, "ignored a Zalo update");
            return;
        };
        let Some(sink) = self.sink.get() else {
            tracing::warn!(channel = %self.id, "event before the channel started; dropped");
            return;
        };
        if sink.deliver(event).await.is_err() {
            tracing::debug!(channel = %self.id, "runtime stopped; event dropped");
        }
    }
}

/// A Zalo bot as an okbot [`Channel`].
pub struct ZaloChannel {
    shared: Arc<Shared>,
    caps: ChannelCaps,
    delivery: Delivery,
}

impl std::fmt::Debug for ZaloChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ZaloChannel")
            .field("id", &self.shared.id)
            .finish_non_exhaustive()
    }
}

fn channel_error(error: zalo_bot::Error) -> ChannelError {
    if error.is_unauthorized() {
        ChannelError::Auth(error.to_string())
    } else if error.is_rate_limited() {
        ChannelError::RateLimited { retry_after: None }
    } else if error.is_retryable() {
        ChannelError::Transient(Box::new(error))
    } else {
        ChannelError::Failed(Box::new(error))
    }
}

impl ZaloChannel {
    /// Creates the channel. Nothing is contacted until [`Channel::run`].
    pub fn new(config: ZaloConfig) -> Self {
        Self {
            shared: Arc::new(Shared {
                bot: client(&config.api_base, &config.token),
                id: config.id,
                sink: OnceLock::new(),
            }),
            caps: ChannelCaps {
                max_message_len: 2000,
                length_unit: LengthUnit::Utf16,
                dialect: MarkdownDialect::ZaloMd,
                threads: false,
                edit: false,
                typing: true,
            },
            delivery: config.delivery,
        }
    }

    /// The API client (for `okbot doctor` checks).
    pub fn bot(&self) -> &Bot {
        &self.shared.bot
    }

    async fn poll(
        &self,
        timeout: Duration,
        shutdown: &CancellationToken,
    ) -> Result<(), ChannelError> {
        // getUpdates does not work while a webhook is registered.
        if let Err(error) = self.shared.bot.delete_webhook().await {
            if error.is_unauthorized() {
                return Err(channel_error(error));
            }
            if error.code() == Some(400) {
                // Zalo answers 400 when no webhook is registered: nothing to remove.
                tracing::debug!(channel = %self.shared.id, %error, "no webhook to delete");
            } else {
                tracing::warn!(channel = %self.shared.id, %error, "deleteWebhook failed");
            }
        }
        tracing::info!(channel = %self.shared.id, "polling Zalo for messages");
        let options = PollOptions::default().timeout(timeout).delete_webhook(true);
        let mut updates = std::pin::pin!(self.shared.bot.updates(options));
        loop {
            let next = tokio::select! {
                () = shutdown.cancelled() => return Ok(()),
                next = updates.next() => next,
            };
            match next {
                Some(Ok(update)) => self.shared.deliver(&update).await,
                // The stream retries these itself, after a backoff.
                Some(Err(error)) if error.is_retryable() => {
                    tracing::warn!(channel = %self.shared.id, %error, "getUpdates failed; retrying");
                }
                Some(Err(error)) => return Err(channel_error(error)),
                None => return Ok(()),
            }
        }
    }

    async fn register_webhook(
        &self,
        url: &str,
        secret: &Secret<String>,
    ) -> Result<(), ChannelError> {
        let set = self
            .shared
            .bot
            .set_webhook(url, secret.expose().as_str())
            .await
            .map_err(channel_error)?;
        match set.verification {
            Some(check) if !check.ok => tracing::warn!(
                channel = %self.shared.id, outcome = %check.outcome, hint = %check.hint,
                "Zalo cannot reach the webhook yet"
            ),
            _ => tracing::info!(channel = %self.shared.id, %url, "Zalo webhook registered"),
        }
        Ok(())
    }
}

#[async_trait]
impl Channel for ZaloChannel {
    fn id(&self) -> &ChannelId {
        &self.shared.id
    }

    fn caps(&self) -> &ChannelCaps {
        &self.caps
    }

    async fn run(
        &self,
        sink: Arc<dyn InboundSink>,
        shutdown: CancellationToken,
    ) -> Result<(), ChannelError> {
        if self.shared.sink.set(sink).is_err() {
            return Err(ChannelError::Unsupported(
                "a Zalo channel can only run once",
            ));
        }
        match &self.delivery {
            Delivery::Polling { timeout } => self.poll(*timeout, &shutdown).await,
            Delivery::Webhook { public_url, secret } => {
                self.register_webhook(public_url, secret).await?;
                shutdown.cancelled().await;
                Ok(())
            }
        }
    }

    async fn send(
        &self,
        to: &ReplyTarget,
        message: &RenderedMessage,
    ) -> Result<SentRef, ChannelError> {
        let sent = self
            .shared
            .bot
            .send_markdown(to.conversation.room.as_str(), message.text.as_str())
            .await
            .map_err(channel_error)?;
        Ok(SentRef {
            target: to.clone(),
            message_id: MessageId::new(sent.message_id),
        })
    }

    async fn typing(&self, to: &ReplyTarget) -> Result<(), ChannelError> {
        self.shared
            .bot
            .send_chat_action(to.conversation.room.as_str(), ChatAction::Typing)
            .await
            .map_err(channel_error)
    }

    fn webhook(&self) -> Option<Arc<dyn WebhookHandler>> {
        match &self.delivery {
            Delivery::Webhook { secret, .. } => Some(Arc::new(Webhook {
                shared: self.shared.clone(),
                secret: secret.clone(),
            })),
            Delivery::Polling { .. } => None,
        }
    }
}

/// Receives Zalo webhook calls.
struct Webhook {
    shared: Arc<Shared>,
    secret: Secret<String>,
}

#[async_trait]
impl WebhookHandler for Webhook {
    fn path(&self) -> String {
        format!("zalo/{}", self.shared.id.instance())
    }

    async fn handle(&self, request: WebhookRequest) -> WebhookResponse {
        if request.method != "POST" {
            return WebhookResponse::status(405);
        }
        let presented = request.header(zalo_bot::webhook::SECRET_HEADER);
        if !zalo_bot::webhook::verify_secret(presented.map(str::as_bytes), self.secret.expose()) {
            tracing::warn!(channel = %self.shared.id, "webhook call with a wrong secret");
            return WebhookResponse::status(403);
        }
        let update = match zalo_bot::webhook::parse_update(&request.body) {
            Ok(update) => update,
            Err(error) => {
                // A malformed body or an event shape this version does not know.
                tracing::debug!(channel = %self.shared.id, %error, "unreadable Zalo update");
                return WebhookResponse::json(&serde_json::json!({ "message": "ignored" }));
            }
        };
        // Delivering only enqueues the event; the agent runs later, so Zalo gets a fast 200.
        self.shared.deliver(&update).await;
        WebhookResponse::json(&serde_json::json!({ "message": "ok" }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_match_the_platform() {
        let c = ZaloChannel::new(ZaloConfig::polling(
            "zalo:shop".parse().unwrap(),
            Secret::new("1:x".into()),
        ));
        assert_eq!(c.caps().max_message_len, 2000);
        assert_eq!(c.caps().dialect, MarkdownDialect::ZaloMd);
        assert!(c.caps().typing && !c.caps().edit);
        assert!(c.webhook().is_none());
    }
}
