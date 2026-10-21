//! Slack channel for okbot (Socket Mode).
//!
//! Socket Mode needs no public URL: okbot opens a WebSocket with the app-level token
//! (`xapp-…`), acknowledges every envelope at once (Slack's 3-second rule) and answers with the
//! Web API using the bot token (`xoxb-…`). The app subscribes to `app_mention` and `message.im`;
//! replies go into the message's thread. Prompts are Block Kit buttons; their presses arrive as
//! `interactive` envelopes (Interactivity must be on, which Socket Mode needs no URL for).

mod event;
mod prompt;
mod web;

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use futures::{SinkExt as _, StreamExt as _};
use okbot_core::{
    CancellationToken, Channel, ChannelCaps, ChannelError, ChannelId, InboundSink, LengthUnit,
    MarkdownDialect, MessageId, Prompt, RenderedMessage, ReplyTarget, Secret, SentRef, async_trait,
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
    /// Prompts with buttons, by prompt id.
    prompts: std::sync::Mutex<std::collections::HashMap<String, prompt::Open>>,
}

fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
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
            prompts: std::sync::Mutex::default(),
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
                Some("interactive") if !self.pressed(&envelope["payload"], sink.as_ref()).await => {
                    return Ok(Next::Stop);
                }
                _ => {}
            }
        }
    }

    /// A `block_actions` payload for one of okbot's prompts: checked against the prompt's allowed
    /// people, then handed to the runtime. Returns `false` when the runtime stopped.
    async fn pressed(&self, payload: &Value, sink: &dyn InboundSink) -> bool {
        if payload["type"].as_str() != Some("block_actions") {
            return true;
        }
        let Some((prompt, button)) = payload["actions"][0]["action_id"]
            .as_str()
            .and_then(prompt::parse)
        else {
            return true;
        };
        let user = okbot_core::UserRef {
            id: okbot_core::UserId::new(payload["user"]["id"].as_str().unwrap_or_default()),
            display_name: payload["user"]["name"]
                .as_str()
                .or(payload["user"]["username"].as_str())
                .unwrap_or_default()
                .to_string(),
            is_bot: false,
        };
        let found = lock(&self.prompts).get(prompt).map(|open| {
            (
                open.allowed.is_empty() || open.allowed.contains(&user.id),
                open.target.clone(),
                open.ts.clone(),
            )
        });
        let Some((allowed, target, ts)) = found else {
            return true;
        };
        if !allowed {
            let conversation = &target.conversation;
            if let Err(error) = self
                .web
                .post_ephemeral(
                    &conversation.room,
                    user.id.as_str(),
                    conversation.thread.as_deref(),
                    "⛔ This is not yours to answer.",
                )
                .await
            {
                tracing::debug!(channel = %self.id, %error, "cannot tell a presser it is not theirs");
            }
            return true;
        }
        let answer = okbot_core::PromptAnswer {
            prompt: prompt.to_string(),
            button: button.to_string(),
            values: std::collections::BTreeMap::new(),
            user,
            conversation: target.conversation.clone(),
            sent: SentRef {
                target,
                message_id: MessageId::new(ts),
            },
        };
        sink.answer(answer).await.is_ok()
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
                None,
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
                None,
            )
            .await
            .map_err(channel_error)
    }

    async fn prompt(&self, to: &ReplyTarget, prompt: &Prompt) -> Result<SentRef, ChannelError> {
        // Modals need a trigger id from a fresh interaction; forms are not offered here.
        if prompt.buttons.iter().any(|b| b.form.is_some()) {
            return Err(ChannelError::Unsupported("forms"));
        }
        let blocks = prompt::blocks(&prompt.text, Some(prompt));
        let ts = self
            .web
            .post_message(
                &to.conversation.room,
                to.conversation.thread.as_deref(),
                &prompt.text,
                Some(&blocks),
            )
            .await
            .map_err(channel_error)?;
        lock(&self.prompts).insert(
            prompt.id.clone(),
            prompt::Open {
                allowed: prompt.allowed.clone(),
                prompt: prompt.clone(),
                target: to.clone(),
                ts: ts.clone(),
            },
        );
        Ok(SentRef {
            target: to.clone(),
            message_id: MessageId::new(ts),
        })
    }

    async fn close_prompt(
        &self,
        sent: &SentRef,
        message: &RenderedMessage,
        keep_buttons: bool,
    ) -> Result<(), ChannelError> {
        let kept = {
            let mut prompts = lock(&self.prompts);
            let id = prompts
                .iter()
                .find(|(_, p)| p.ts == sent.message_id.as_str())
                .map(|(id, _)| id.clone());
            match id {
                Some(id) if keep_buttons => prompts.get(&id).map(|p| p.prompt.clone()),
                Some(id) => {
                    prompts.remove(&id);
                    None
                }
                None => None,
            }
        };
        let blocks = prompt::blocks(&message.text, kept.as_ref());
        self.web
            .update(
                &sent.target.conversation.room,
                sent.message_id.as_str(),
                &message.text,
                Some(&blocks),
            )
            .await
            .map_err(channel_error)
    }
}

#[cfg(test)]
mod tests;
