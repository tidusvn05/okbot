//! Google Chat channel for okbot (HTTP endpoint and Pub/Sub).
//!
//! A Chat app receives events either on an HTTPS endpoint (Google `POST`s each interaction,
//! authenticated with a Google-signed bearer token) or through a Cloud Pub/Sub pull subscription
//! (no public URL). Either way okbot answers asynchronously with the Chat REST API as the app's
//! service account, because agent turns take longer than Chat's 30-second synchronous window
//! (design §3.3.1).

mod api;
mod auth;
mod event;
mod jwt;
mod pubsub;
mod verify;

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use okbot_core::{
    CancellationToken, Channel, ChannelCaps, ChannelError, ChannelId, InboundSink, LengthUnit,
    MarkdownDialect, MessageId, RenderedMessage, ReplyTarget, SentRef, WebhookHandler,
    WebhookRequest, WebhookResponse, async_trait,
};

pub use api::{ApiError, CHAT_API, ChatApi};
pub use auth::{AuthError, SCOPES, ServiceAccount, TokenSource};
pub use pubsub::{PUBSUB_API, Subscription};
pub use verify::{Audience, CHAT_ISSUER, Verifier};

/// How the channel receives events.
#[derive(Debug, Clone)]
pub enum Delivery {
    /// Google calls okbot's HTTPS endpoint.
    Http {
        /// The app's "Authentication audience" setting.
        audience: Audience,
    },
    /// okbot pulls from a Pub/Sub subscription (`projects/<p>/subscriptions/<s>`).
    PubSub {
        /// Subscription name.
        subscription: String,
    },
}

/// Overridable Google endpoints (tests point them at fakes).
#[derive(Debug, Clone)]
pub struct Endpoints {
    /// Chat API base.
    pub chat: String,
    /// Pub/Sub API base.
    pub pubsub: String,
    /// OAuth token endpoint; `None` keeps the key file's `token_uri`.
    pub token: Option<String>,
    /// JWK set for request verification; `None` uses Google's.
    pub jwks: Option<String>,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            chat: CHAT_API.into(),
            pubsub: PUBSUB_API.into(),
            token: None,
            jwks: None,
        }
    }
}

/// Settings of one Chat app.
#[derive(Debug, Clone)]
pub struct GChatConfig {
    /// Channel id, `gchat:<instance>`.
    pub id: ChannelId,
    /// The app's service account.
    pub account: ServiceAccount,
    /// Receiving mode.
    pub delivery: Delivery,
    /// Google endpoints.
    pub endpoints: Endpoints,
}

struct Shared {
    id: ChannelId,
    sink: OnceLock<Arc<dyn InboundSink>>,
}

impl Shared {
    async fn deliver(&self, event: &serde_json::Value) {
        let Some(inbound) = event::to_inbound(&self.id, event, jiff::Timestamp::now()) else {
            return;
        };
        match self.sink.get() {
            Some(sink) => {
                if sink.deliver(inbound).await.is_err() {
                    tracing::debug!(channel = %self.id, "runtime stopped; event dropped");
                }
            }
            None => tracing::warn!(channel = %self.id, "event before the channel started; dropped"),
        }
    }
}

/// A Google Chat app as an okbot [`Channel`].
pub struct GChatChannel {
    shared: Arc<Shared>,
    api: ChatApi,
    caps: ChannelCaps,
    delivery: Delivery,
    subscription: Option<Subscription>,
    verifier: Option<Verifier>,
}

impl std::fmt::Debug for GChatChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GChatChannel")
            .field("id", &self.shared.id)
            .finish_non_exhaustive()
    }
}

fn channel_error(error: ApiError) -> ChannelError {
    match error {
        ApiError::Auth(AuthError::Refused(m) | AuthError::Credentials(m)) => ChannelError::Auth(m),
        ApiError::Http {
            status: 401 | 403,
            message,
        } => ChannelError::Auth(message),
        ApiError::Http { status: 429, .. } => ChannelError::RateLimited { retry_after: None },
        e if e.is_retryable() => ChannelError::Transient(Box::new(e)),
        e => ChannelError::Failed(Box::new(e)),
    }
}

impl GChatChannel {
    /// Creates the channel. Nothing is contacted until [`Channel::run`].
    pub fn new(config: GChatConfig) -> Self {
        // reqwest is built without a crypto provider: okbot uses ring only (no-op when installed).
        let _ = rustls::crypto::ring::default_provider().install_default();
        let http = reqwest::Client::builder()
            .user_agent(concat!("okbot/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_default();
        let account = match &config.endpoints.token {
            Some(uri) => config.account.with_token_uri(uri),
            None => config.account,
        };
        let tokens = TokenSource::new(account, http.clone());
        let api = ChatApi {
            http: http.clone(),
            base: config.endpoints.chat.trim_end_matches('/').into(),
            tokens: tokens.clone(),
        };
        let (subscription, verifier) = match &config.delivery {
            Delivery::PubSub { subscription } => (
                Some(Subscription {
                    http,
                    base: config.endpoints.pubsub.trim_end_matches('/').into(),
                    tokens,
                    name: subscription.clone(),
                }),
                None,
            ),
            Delivery::Http { audience } => (
                None,
                Some(Verifier::new(
                    audience.clone(),
                    config.endpoints.jwks.clone(),
                    http,
                )),
            ),
        };
        Self {
            shared: Arc::new(Shared {
                id: config.id,
                sink: OnceLock::new(),
            }),
            api,
            caps: ChannelCaps {
                max_message_len: 4096,
                length_unit: LengthUnit::Chars,
                dialect: MarkdownDialect::GChatText,
                threads: true,
                edit: true,
                typing: false,
            },
            delivery: config.delivery,
            subscription,
            verifier,
        }
    }

    /// The Chat API (for `okbot doctor`).
    pub fn api(&self) -> &ChatApi {
        &self.api
    }

    async fn pull_loop(
        &self,
        sub: &Subscription,
        shutdown: &CancellationToken,
    ) -> Result<(), ChannelError> {
        tracing::info!(channel = %self.shared.id, subscription = %sub.name, "pulling Google Chat events");
        let mut failures: u32 = 0;
        loop {
            let pulled = tokio::select! {
                () = shutdown.cancelled() => return Ok(()),
                r = sub.pull() => r,
            };
            match pulled {
                Ok(messages) if messages.is_empty() => {
                    failures = 0;
                    tokio::select! {
                        () = shutdown.cancelled() => return Ok(()),
                        () = tokio::time::sleep(Duration::from_millis(500)) => {}
                    }
                }
                Ok(messages) => {
                    failures = 0;
                    let mut acks = Vec::with_capacity(messages.len());
                    for (ack, event) in messages {
                        // Delivering stores nothing yet but queues the event; at-least-once
                        // redelivery is caught by the runtime's dedupe.
                        self.shared.deliver(&event).await;
                        acks.push(ack);
                    }
                    if let Err(error) = sub.ack(&acks).await {
                        tracing::warn!(channel = %self.shared.id, %error, "Pub/Sub ack failed");
                    }
                }
                Err(error) if !error.is_retryable() => return Err(channel_error(error)),
                Err(error) => {
                    let wait = Duration::from_secs(1u64 << failures.min(5));
                    failures = failures.saturating_add(1);
                    tracing::warn!(channel = %self.shared.id, %error, ?wait, "Pub/Sub pull failed");
                    tokio::select! {
                        () = shutdown.cancelled() => return Ok(()),
                        () = tokio::time::sleep(wait) => {}
                    }
                }
            }
        }
    }
}

#[async_trait]
impl Channel for GChatChannel {
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
                "a Google Chat channel can only run once",
            ));
        }
        // Fail early on bad credentials.
        self.api
            .tokens
            .token()
            .await
            .map_err(|e| channel_error(e.into()))?;
        match (&self.delivery, &self.subscription) {
            (Delivery::PubSub { .. }, Some(sub)) => self.pull_loop(sub, &shutdown).await,
            _ => {
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
        let name = self
            .api
            .create_message(
                &to.conversation.room,
                to.conversation.thread.as_deref(),
                &message.text,
            )
            .await
            .map_err(channel_error)?;
        Ok(SentRef {
            target: to.clone(),
            message_id: MessageId::new(name),
        })
    }

    async fn edit(&self, sent: &SentRef, message: &RenderedMessage) -> Result<(), ChannelError> {
        self.api
            .update_message(sent.message_id.as_str(), &message.text)
            .await
            .map_err(channel_error)
    }

    fn webhook(&self) -> Option<Arc<dyn WebhookHandler>> {
        self.verifier.as_ref().map(|verifier| {
            Arc::new(Webhook {
                shared: self.shared.clone(),
                verifier: verifier.clone(),
            }) as Arc<dyn WebhookHandler>
        })
    }
}

struct Webhook {
    shared: Arc<Shared>,
    verifier: Verifier,
}

#[async_trait]
impl WebhookHandler for Webhook {
    fn path(&self) -> String {
        format!("gchat/{}", self.shared.id.instance())
    }

    async fn handle(&self, request: WebhookRequest) -> WebhookResponse {
        if request.method != "POST" {
            return WebhookResponse::status(405);
        }
        if let Err(reason) = self.verifier.check(request.header("authorization")).await {
            tracing::warn!(channel = %self.shared.id, %reason, "rejected a Google Chat request");
            return WebhookResponse::status(401);
        }
        let Ok(event) = serde_json::from_slice::<serde_json::Value>(&request.body) else {
            return WebhookResponse::status(400);
        };
        self.shared.deliver(&event).await;
        // An empty answer: the reply follows through the Chat API.
        WebhookResponse::json(&serde_json::json!({}))
    }
}

#[cfg(test)]
mod tests;
