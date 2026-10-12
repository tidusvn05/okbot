//! Zalo Bot Platform channel for okbot (long polling and webhook).
//!
//! [Zalo Bot Platform](https://bot.zapps.me/docs/) runs an automated account inside the Zalo app.
//! This channel receives messages either by long polling (`getUpdates`, no public URL needed) or
//! by webhook (`setWebhook`, a public HTTPS URL), and replies with `sendMessage`.
//!
//! Notes from the platform (design §3.3.2):
//! - the bot token is part of every API URL, so URLs are never logged;
//! - messages hold at most 2000 characters, support a small Markdown subset, cannot be edited;
//! - group messages are delivered only when they mention or reply to the bot;
//! - `message.unsupported.received` replaces messages from protected users and is answered with a
//!   fixed reply, never processed.

mod api;
mod event;

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use okbot_core::{
    CancellationToken, Channel, ChannelCaps, ChannelError, ChannelId, InboundSink, LengthUnit,
    MarkdownDialect, MessageId, RenderedMessage, ReplyTarget, Secret, SentRef, WebhookHandler,
    WebhookRequest, WebhookResponse, async_trait,
};

pub use api::{ApiError, BotInfo, DEFAULT_API_BASE, WebhookCheck, ZaloApi};

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
    api: ZaloApi,
    sink: OnceLock<Arc<dyn InboundSink>>,
}

impl Shared {
    async fn deliver(&self, update: &serde_json::Value) {
        let Some(event) = event::to_inbound(&self.id, update, jiff::Timestamp::now()) else {
            tracing::debug!(channel = %self.id, "ignored a Zalo update");
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

fn channel_error(error: ApiError) -> ChannelError {
    match error {
        ApiError::Unauthorized(message) => ChannelError::Auth(message),
        ApiError::RateLimited(_) => ChannelError::RateLimited { retry_after: None },
        e if e.is_retryable() => ChannelError::Transient(Box::new(e)),
        e => ChannelError::Failed(Box::new(e)),
    }
}

/// Exponential backoff with jitter: 1 s, 2 s, 4 s … up to 60 s.
fn backoff(attempt: u32) -> Duration {
    let base = 1000u64.saturating_mul(1 << attempt.min(6)).min(60_000);
    let jitter = rand::random::<u64>() % (base / 4 + 1);
    Duration::from_millis(base + jitter)
}

impl ZaloChannel {
    /// Creates the channel. Nothing is contacted until [`Channel::run`].
    pub fn new(config: ZaloConfig) -> Self {
        Self {
            shared: Arc::new(Shared {
                api: ZaloApi::new(&config.api_base, config.token),
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
    pub fn api(&self) -> &ZaloApi {
        &self.shared.api
    }

    async fn poll(
        &self,
        timeout: Duration,
        shutdown: &CancellationToken,
    ) -> Result<(), ChannelError> {
        // getUpdates does not work while a webhook is registered.
        if let Err(error) = self.shared.api.delete_webhook().await {
            if matches!(error, ApiError::Unauthorized(_)) {
                return Err(channel_error(error));
            }
            tracing::warn!(channel = %self.shared.id, %error, "deleteWebhook failed");
        }
        tracing::info!(channel = %self.shared.id, "polling Zalo for messages");
        let mut failures = 0u32;
        loop {
            let updates = tokio::select! {
                () = shutdown.cancelled() => return Ok(()),
                r = self.shared.api.get_updates(timeout) => r,
            };
            match updates {
                Ok(updates) => {
                    failures = 0;
                    for update in &updates {
                        self.shared.deliver(update).await;
                    }
                }
                Err(error @ ApiError::Unauthorized(_)) => return Err(channel_error(error)),
                Err(error) => {
                    let wait = backoff(failures);
                    failures = failures.saturating_add(1);
                    tracing::warn!(channel = %self.shared.id, %error, ?wait, "getUpdates failed");
                    tokio::select! {
                        () = shutdown.cancelled() => return Ok(()),
                        () = tokio::time::sleep(wait) => {}
                    }
                }
            }
        }
    }

    async fn register_webhook(
        &self,
        url: &str,
        secret: &Secret<String>,
    ) -> Result<(), ChannelError> {
        match self.shared.api.set_webhook(url, secret).await {
            Ok(Some(check)) if !check.ok => tracing::warn!(
                channel = %self.shared.id, outcome = %check.outcome, hint = %check.hint,
                "Zalo cannot reach the webhook yet"
            ),
            Ok(_) => tracing::info!(channel = %self.shared.id, %url, "Zalo webhook registered"),
            Err(error) => return Err(channel_error(error)),
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
        let id = self
            .shared
            .api
            .send_message(&to.conversation.room, &message.text)
            .await
            .map_err(channel_error)?;
        Ok(SentRef {
            target: to.clone(),
            message_id: MessageId::new(id),
        })
    }

    async fn typing(&self, to: &ReplyTarget) -> Result<(), ChannelError> {
        self.shared
            .api
            .typing(&to.conversation.room)
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

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
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
        let presented = request.header("x-bot-api-secret-token").unwrap_or_default();
        if !constant_time_eq(presented.as_bytes(), self.secret.expose().as_bytes()) {
            tracing::warn!(channel = %self.shared.id, "webhook call with a wrong secret");
            return WebhookResponse::status(403);
        }
        let Ok(update) = serde_json::from_slice::<serde_json::Value>(&request.body) else {
            return WebhookResponse::status(400);
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
    fn backoff_grows_and_caps() {
        assert!(backoff(0) >= Duration::from_secs(1) && backoff(0) < Duration::from_secs(2));
        assert!(backoff(3) >= Duration::from_secs(8));
        assert!(backoff(30) <= Duration::from_secs(75));
    }

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
