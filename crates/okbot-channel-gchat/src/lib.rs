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
mod prompt;
mod pubsub;
mod verify;

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use okbot_core::{
    CancellationToken, Channel, ChannelCaps, ChannelError, ChannelId, ConversationKey, InboundSink,
    LengthUnit, MarkdownDialect, MessageId, Prompt, RenderedMessage, ReplyTarget, SentRef,
    WebhookHandler, WebhookRequest, WebhookResponse, async_trait,
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
    /// Prompts with buttons, by prompt id.
    prompts: std::sync::Mutex<std::collections::HashMap<String, prompt::Open>>,
    /// Dialogs (forms) can be opened: HTTP delivery answers clicks synchronously.
    dialogs: bool,
}

fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Shared {
    /// The open prompt `click` is for: from memory, or from the runtime after a restart.
    async fn open(&self, click: &event::Click, sink: &dyn InboundSink) -> Option<prompt::Open> {
        if let Some(open) = lock(&self.prompts).get(&click.prompt) {
            return Some(open.clone());
        }
        // Recovering needs to know where the prompt is, which the event tells.
        let place = click.place.as_ref()?;
        let target = ReplyTarget {
            conversation: ConversationKey {
                channel: self.id.clone(),
                space: None,
                room: place.room.clone(),
                thread: place.thread.clone(),
            },
            in_reply_to: None,
        };
        let recovered = sink.prompt(&click.prompt, &target.conversation).await?;
        if !recovered.is_answerable() {
            return None;
        }
        let open = prompt::Open::new(&recovered, target, place.message.clone());
        lock(&self.prompts).insert(click.prompt.clone(), open.clone());
        Some(open)
    }

    /// A click on a prompt: checked against the prompt's allowed people, then handed to the
    /// runtime. Returns the synchronous answer for dialogs (open, close, or an error notice).
    /// Others' plain clicks are ignored (Chat has no private notice for them here).
    async fn clicked(&self, click: event::Click) -> Option<serde_json::Value> {
        let sink = self.sink.get()?;
        let add_on = click.add_on;
        let Some(open) = self.open(&click, sink.as_ref()).await else {
            tracing::debug!(channel = %self.id, "a click on an unknown or expired prompt");
            return click
                .submit
                .then(|| prompt::dialog_error(add_on, "This prompt has expired."));
        };
        let button = click.button.as_deref().and_then(|b| open.prompt.button(b));
        let form = button.and_then(|b| b.form.as_ref());
        let dialog = click.submit || form.is_some();
        if !open.prompt.may_answer(&click.user.id) {
            tracing::info!(channel = %self.id, user = %click.user.id, "a prompt was clicked by someone it is not for");
            return dialog.then(|| prompt::dialog_error(add_on, "This prompt is not for you."));
        }
        let Some(id) = click.button.filter(|b| open.prompt.answers_with(b)) else {
            return click
                .submit
                .then(|| prompt::dialog_error(add_on, "This prompt has changed."));
        };
        let values = match (form, click.submit) {
            (Some(form), true) => match prompt::form_values(form, &click.inputs) {
                Ok(values) => values,
                Err(message) => return Some(prompt::dialog_error(add_on, &message)),
            },
            (Some(form), false) => {
                if !self.dialogs {
                    tracing::warn!(channel = %self.id, "a form needs HTTP delivery; click ignored");
                    return None;
                }
                let card = prompt::dialog(&open.prompt, button?, form);
                return Some(prompt::open_dialog(add_on, &card));
            }
            (None, true) => {
                return Some(prompt::dialog_error(add_on, "This prompt has changed."));
            }
            (None, false) => std::collections::BTreeMap::new(),
        };
        let answer = okbot_core::PromptAnswer {
            prompt: click.prompt,
            button: id,
            values,
            user: click.user,
            conversation: open.target.conversation.clone(),
            sent: SentRef {
                target: open.target,
                message_id: MessageId::new(open.name),
            },
        };
        if sink.answer(answer).await.is_err() {
            tracing::debug!(channel = %self.id, "runtime stopped; answer dropped");
        }
        dialog.then(|| prompt::close_dialog(add_on))
    }

    /// Handles one event; returns the synchronous answer, if the event needs one.
    async fn deliver(&self, event: &serde_json::Value) -> Option<serde_json::Value> {
        if let Some(add_on) = event::dialog_cancelled(event) {
            return Some(prompt::close_dialog(add_on));
        }
        if let Some(click) = event::to_click(event) {
            return self.clicked(click).await;
        }
        let inbound = event::to_inbound(&self.id, event, jiff::Timestamp::now())?;
        match self.sink.get() {
            Some(sink) => {
                if sink.deliver(inbound).await.is_err() {
                    tracing::debug!(channel = %self.id, "runtime stopped; event dropped");
                }
            }
            None => tracing::warn!(channel = %self.id, "event before the channel started; dropped"),
        }
        None
    }

    /// Forgets the open prompt shown in message `name`.
    fn forget(&self, name: &str) {
        lock(&self.prompts).retain(|_, p| p.name != name);
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
                prompts: std::sync::Mutex::default(),
                dialogs: matches!(config.delivery, Delivery::Http { .. }),
            }),
            api,
            caps: ChannelCaps {
                max_message_len: 4096,
                length_unit: LengthUnit::Chars,
                dialect: MarkdownDialect::GChatText,
                threads: true,
                edit: true,
                typing: false,
                ui: okbot_core::UiCaps {
                    buttons: true,
                    select: true,
                    // Dialogs open only as the synchronous answer to a click.
                    forms: matches!(config.delivery, Delivery::Http { .. }),
                    cards: true,
                    // Card images need a public URL (ADR 0016).
                    images: false,
                },
            },
            delivery: config.delivery,
            subscription,
            verifier,
        }
    }

    /// Dialogs need a synchronous answer to a click: forms only with HTTP delivery.
    fn check_forms(&self, prompt: &Prompt) -> Result<(), ChannelError> {
        if !self.shared.dialogs && prompt.buttons.iter().any(|b| b.form.is_some()) {
            return Err(ChannelError::Unsupported("forms"));
        }
        Ok(())
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
                        // Pub/Sub has no synchronous answer (no dialogs).
                        let _ = self.shared.deliver(&event).await;
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
                None,
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
            .update_message(sent.message_id.as_str(), &message.text, None)
            .await
            .map_err(channel_error)
    }

    async fn prompt(&self, to: &ReplyTarget, prompt: &Prompt) -> Result<SentRef, ChannelError> {
        self.check_forms(prompt)?;
        let cards = prompt::cards(prompt);
        let name = self
            .api
            .create_message(
                &to.conversation.room,
                to.conversation.thread.as_deref(),
                &prompt.text,
                Some(&cards),
            )
            .await
            .map_err(channel_error)?;
        lock(&self.shared.prompts).insert(
            prompt.id.clone(),
            prompt::Open {
                prompt: prompt.clone(),
                cards,
                target: to.clone(),
                name: name.clone(),
            },
        );
        Ok(SentRef {
            target: to.clone(),
            message_id: MessageId::new(name),
        })
    }

    async fn close_prompt(
        &self,
        sent: &SentRef,
        message: &RenderedMessage,
        keep_buttons: bool,
    ) -> Result<(), ChannelError> {
        let kept = {
            let mut prompts = lock(&self.shared.prompts);
            let id = prompts
                .iter()
                .find(|(_, p)| p.name == sent.message_id.as_str())
                .map(|(id, _)| id.clone());
            match id {
                Some(id) if keep_buttons => prompts.get(&id).map(|p| p.cards.clone()),
                Some(id) => {
                    prompts.remove(&id);
                    None
                }
                None => None,
            }
        };
        let cards = kept.unwrap_or_else(|| serde_json::json!([]));
        self.api
            .update_message(sent.message_id.as_str(), &message.text, Some(&cards))
            .await
            .map_err(channel_error)
    }

    async fn update_prompt(&self, sent: &SentRef, prompt: &Prompt) -> Result<(), ChannelError> {
        self.check_forms(prompt)?;
        let name = sent.message_id.as_str();
        let open = prompt::Open::new(prompt, sent.target.clone(), name.to_string());
        self.api
            .update_message(name, &prompt.text, Some(&open.cards))
            .await
            .map_err(channel_error)?;
        self.shared.forget(name);
        if prompt.is_answerable() {
            lock(&self.shared.prompts).insert(prompt.id.clone(), open);
        }
        Ok(())
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
        match self.shared.deliver(&event).await {
            // Dialogs open and close in the synchronous answer.
            Some(answer) => WebhookResponse::json(&answer),
            // An empty answer: the reply follows through the Chat API.
            None => WebhookResponse::json(&serde_json::json!({})),
        }
    }
}

#[cfg(test)]
mod tests;
