//! Telegram Bot API channel for okbot (long polling and webhook).
//!
//! A bot made with [@BotFather](https://t.me/BotFather) receives messages either by long polling
//! (`getUpdates`, no public URL needed) or by webhook (`setWebhook`, a public HTTPS URL with a
//! secret token), and replies with `sendMessage` in `parse_mode = "HTML"`.
//!
//! Notes from the platform:
//! - the token is part of every API URL; [`api::TelegramApi`] keeps it out of `Debug` output and
//!   errors, and file download URLs (which contain it too) only reach okbot's own download code;
//! - messages hold at most 4096 UTF-16 code units and can be edited; with a typing indicator, the
//!   runtime shows "typing…" and sends the final answer;
//! - in groups, privacy mode (on by default) delivers only commands, replies to the bot and
//!   mentions; turn it off in @BotFather for `reply_on = ["all"]`;
//! - forum topics are threads (`message_thread_id`);
//! - there is no card widget: cards are bold-titled blocks of text, and a card's chart (ADR 0016)
//!   is a photo (`sendPhoto`) with the text as its caption and the buttons under it.

pub mod api;
mod event;
mod prompt;

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use okbot_core::{
    Attachment, CancellationToken, Channel, ChannelCaps, ChannelError, ChannelId, ConversationKey,
    InboundSink, LengthUnit, MarkdownDialect, MessageId, Prompt, RenderedMessage, ReplyTarget,
    Secret, SentRef, UserId, WebhookHandler, WebhookRequest, WebhookResponse, async_trait,
};

pub use api::{API_BASE, TelegramApi, TelegramError};
use event::{Bot, PendingFile, Update};

/// The header Telegram sends the webhook secret in.
pub const SECRET_HEADER: &str = "x-telegram-bot-api-secret-token";

/// How the channel receives updates. Telegram allows one of the two at a time.
#[derive(Debug, Clone)]
pub enum Delivery {
    /// `getUpdates` long polling. Works behind NAT.
    Polling {
        /// Long-poll timeout per request.
        timeout: Duration,
    },
    /// Telegram `POST`s updates to `public_url`, authenticated by `secret`.
    Webhook {
        /// The HTTPS URL Telegram calls (it reaches okbot's HTTP server through a reverse proxy).
        public_url: String,
        /// Sent back by Telegram in `X-Telegram-Bot-Api-Secret-Token`.
        secret: Secret<String>,
    },
}

/// Settings of one Telegram bot.
#[derive(Debug, Clone)]
pub struct TelegramConfig {
    /// Channel id, `telegram:<instance>`.
    pub id: ChannelId,
    /// Bot token from @BotFather.
    pub token: Secret<String>,
    /// Receiving mode.
    pub delivery: Delivery,
    /// API base URL (tests point it at a fake server).
    pub api_base: String,
}

impl TelegramConfig {
    /// Polling with a 30-second long poll against the production API.
    pub fn polling(id: ChannelId, token: Secret<String>) -> Self {
        Self {
            id,
            token,
            delivery: Delivery::Polling {
                timeout: Duration::from_secs(30),
            },
            api_base: API_BASE.to_string(),
        }
    }
}

/// Whether `secret` is a valid webhook secret token for Telegram (1–256 of `A-Z a-z 0-9 _ -`).
pub fn valid_secret(secret: &str) -> bool {
    (1..=256).contains(&secret.len())
        && secret
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// State shared by the channel and its webhook handler.
struct Shared {
    id: ChannelId,
    api: TelegramApi,
    me: OnceLock<Bot>,
    sink: OnceLock<Arc<dyn InboundSink>>,
    /// Prompts with buttons, by prompt id.
    prompts: std::sync::Mutex<std::collections::HashMap<String, prompt::Open>>,
}

fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Shared {
    /// Converts and delivers one raw update. Returns `false` when the runtime stopped.
    async fn deliver(&self, raw: serde_json::Value) -> bool {
        let update: Update = match serde_json::from_value(raw) {
            Ok(update) => update,
            Err(error) => {
                tracing::debug!(channel = %self.id, %error, "unreadable Telegram update");
                return true;
            }
        };
        let (Some(me), Some(sink)) = (self.me.get(), self.sink.get()) else {
            tracing::warn!(channel = %self.id, "update before the channel started; dropped");
            return true;
        };
        if let Some(query) = &update.callback_query {
            return self.pressed(query, sink.as_ref()).await;
        }
        let Some((mut event, files)) =
            event::to_inbound(&self.id, &update, me, jiff::Timestamp::now())
        else {
            tracing::debug!(channel = %self.id, update = update.update_id, "ignored a Telegram update");
            return true;
        };
        for file in files {
            if let Some(attachment) = self.resolve(file).await {
                event.attachments.push(attachment);
            }
        }
        sink.deliver(event).await.is_ok()
    }

    /// A button press: checked against the prompt's allowed people, then handed to the runtime.
    /// A prompt this process does not hold (it restarted since showing it) is asked of the sink.
    async fn pressed(&self, query: &event::CallbackQuery, sink: &dyn InboundSink) -> bool {
        let user = query.user();
        let found = match query.data.as_deref().and_then(prompt::parse) {
            Some((id, button)) => self.open_for(query, id, button, &user.id, sink).await,
            None => None,
        };
        let Some((prompt, button, allowed, target, message_id)) = found else {
            // An old or foreign button: stop its spinner.
            let _ = self
                .api
                .answer_callback(&query.id, Some("⌛ This has expired."))
                .await;
            return true;
        };
        if !allowed {
            let _ = self
                .api
                .answer_callback(&query.id, Some("⛔ This is not yours to answer."))
                .await;
            return true;
        }
        if let Err(error) = self.api.answer_callback(&query.id, None).await {
            tracing::debug!(channel = %self.id, %error, "answerCallbackQuery failed");
        }
        let answer = okbot_core::PromptAnswer {
            prompt,
            button,
            values: std::collections::BTreeMap::new(),
            user,
            conversation: target.conversation.clone(),
            sent: SentRef {
                target,
                message_id: MessageId::new(message_id.to_string()),
            },
        };
        sink.answer(answer).await.is_ok()
    }

    /// The open prompt `id` a press of `button` answers, from memory or else from the sink:
    /// `(prompt, button, may answer, where, message id)`.
    async fn open_for(
        &self,
        query: &event::CallbackQuery,
        id: &str,
        button: &str,
        user: &UserId,
        sink: &dyn InboundSink,
    ) -> Option<(String, String, bool, ReplyTarget, i64)> {
        let check = |open: &prompt::Open| {
            let same_message = query
                .message
                .as_ref()
                .is_none_or(|m| m.message_id == open.message_id);
            (same_message && open.answers.iter().any(|a| a == button)).then(|| {
                (
                    id.to_string(),
                    button.to_string(),
                    open.allowed.is_empty() || open.allowed.contains(user),
                    open.target.clone(),
                    open.message_id,
                )
            })
        };
        if let Some(open) = lock(&self.prompts).get(id) {
            return check(open);
        }
        // Recovering needs the message and its chat, which very old messages no longer carry.
        let message = query.message.as_ref()?;
        let conversation = message.conversation(&self.id)?;
        let prompt = sink.prompt(id, &conversation).await?;
        if !prompt.is_answerable() {
            return None;
        }
        let target = ReplyTarget {
            conversation,
            in_reply_to: None,
        };
        let open = prompt::Open::new(&prompt, target, message.message_id);
        let found = check(&open);
        lock(&self.prompts).insert(id.to_string(), open);
        found
    }

    /// Turns a file id into an attachment with a download URL; `None` when Telegram refuses.
    async fn resolve(&self, file: PendingFile) -> Option<Attachment> {
        let file_id = match &file {
            PendingFile::Image { file_id, .. }
            | PendingFile::File { file_id, .. }
            | PendingFile::Audio { file_id } => file_id.clone(),
        };
        let url = match self.api.file_url(&file_id).await {
            Ok(url) => url,
            Err(error) => {
                tracing::warn!(channel = %self.id, %error, "cannot get a Telegram file");
                return None;
            }
        };
        Some(match file {
            PendingFile::Image { caption, .. } => Attachment::Image { url, caption },
            PendingFile::File { name, .. } => Attachment::File { url, name },
            PendingFile::Audio { .. } => Attachment::Audio { url },
        })
    }
}

/// A Telegram bot as an okbot [`Channel`].
pub struct TelegramChannel {
    shared: Arc<Shared>,
    caps: ChannelCaps,
    delivery: Delivery,
}

impl std::fmt::Debug for TelegramChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelegramChannel")
            .field("id", &self.shared.id)
            .finish_non_exhaustive()
    }
}

fn channel_error(error: TelegramError) -> ChannelError {
    match &error {
        e if e.is_unauthorized() => ChannelError::Auth(error.to_string()),
        TelegramError::Api {
            code: 429,
            retry_after,
            ..
        } => ChannelError::RateLimited {
            retry_after: retry_after.map(Duration::from_secs),
        },
        TelegramError::Api { code, .. } if *code >= 500 => ChannelError::Transient(Box::new(error)),
        TelegramError::Transport(_) => ChannelError::Transient(Box::new(error)),
        TelegramError::Api { .. } => ChannelError::Failed(Box::new(error)),
    }
}

/// The text of rendered HTML, for resending a message Telegram could not parse.
fn html_to_plain(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
}

/// Where a message to `to` goes.
fn destination(to: &ReplyTarget) -> api::Destination<'_> {
    let conversation = &to.conversation;
    api::Destination {
        chat_id: &conversation.room,
        thread: conversation.thread.as_deref(),
        // Quote the question in groups, where several people talk; not in direct chats.
        reply_to: to
            .in_reply_to
            .as_ref()
            .filter(|_| conversation.room.starts_with('-'))
            .map(MessageId::as_str),
    }
}

/// Plain text cut to fit a caption, with an ellipsis when cut.
fn short_caption(plain: &str) -> String {
    if prompt::fits_caption(plain) {
        return plain.to_string();
    }
    let mut out = String::new();
    let mut units = 0;
    for c in plain.chars() {
        // Room for the ellipsis (one unit).
        if units + c.len_utf16() > 1023 {
            break;
        }
        units += c.len_utf16();
        out.push(c);
    }
    out.push('…');
    out
}

/// Compares secrets without an early exit on the first differing byte.
fn same_secret(presented: &[u8], expected: &[u8]) -> bool {
    use sha2::{Digest as _, Sha256};
    let (a, b) = (Sha256::digest(presented), Sha256::digest(expected));
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

impl TelegramChannel {
    /// Creates the channel. Nothing is contacted until [`Channel::run`].
    pub fn new(config: TelegramConfig) -> Self {
        Self {
            shared: Arc::new(Shared {
                api: TelegramApi::new(&config.api_base, config.token),
                id: config.id,
                me: OnceLock::new(),
                sink: OnceLock::new(),
                prompts: std::sync::Mutex::default(),
            }),
            caps: ChannelCaps {
                max_message_len: 4096,
                length_unit: LengthUnit::Utf16,
                dialect: MarkdownDialect::TelegramHtml,
                threads: true,
                edit: true,
                typing: true,
                // Inline keyboards hold buttons, links and a menu (a row per option); there are
                // no forms. Cards are drawn here as text blocks, so that a chart can go with them
                // as a photo.
                ui: okbot_core::UiCaps {
                    buttons: true,
                    select: true,
                    forms: false,
                    cards: true,
                    images: true,
                },
            },
            delivery: config.delivery,
        }
    }

    /// The API client (for `okbot doctor` checks).
    pub fn api(&self) -> &TelegramApi {
        &self.shared.api
    }

    async fn poll(
        &self,
        timeout: Duration,
        shutdown: &CancellationToken,
    ) -> Result<(), ChannelError> {
        // getUpdates answers 409 while a webhook is registered.
        if let Err(error) = self.shared.api.delete_webhook().await {
            if error.is_unauthorized() {
                return Err(channel_error(error));
            }
            tracing::warn!(channel = %self.shared.id, %error, "deleteWebhook failed");
        }
        tracing::info!(channel = %self.shared.id, "polling Telegram for messages");
        let mut offset = 0_i64;
        let mut backoff = Duration::from_secs(1);
        loop {
            let next = tokio::select! {
                () = shutdown.cancelled() => return Ok(()),
                next = self.shared.api.get_updates(offset, timeout) => next,
            };
            let updates = match next {
                Ok(updates) => {
                    backoff = Duration::from_secs(1);
                    updates
                }
                Err(error) if error.is_unauthorized() => return Err(channel_error(error)),
                Err(error) => {
                    // 409: another process polls the same bot, or a webhook was set meanwhile.
                    let wait = match &error {
                        TelegramError::Api {
                            retry_after: Some(s),
                            ..
                        } => Duration::from_secs(*s),
                        _ => backoff,
                    };
                    tracing::warn!(channel = %self.shared.id, %error, "getUpdates failed; retrying");
                    tokio::select! {
                        () = shutdown.cancelled() => return Ok(()),
                        () = tokio::time::sleep(wait) => {}
                    }
                    backoff = (backoff * 2).min(Duration::from_secs(30));
                    continue;
                }
            };
            for raw in updates {
                // Acknowledged by the next call's offset; a crash re-delivers, which the runtime
                // drops as duplicates.
                if let Some(id) = raw.get("update_id").and_then(serde_json::Value::as_i64) {
                    offset = offset.max(id + 1);
                }
                if !self.shared.deliver(raw).await {
                    return Ok(());
                }
            }
        }
    }

    async fn send_text(
        &self,
        to: &ReplyTarget,
        text: &str,
        markup: Option<&serde_json::Value>,
    ) -> Result<i64, TelegramError> {
        let destination = destination(to);
        let api = &self.shared.api;
        match api.send_message(destination, text, true, markup).await {
            Err(error) if error.is_parse_error() => {
                tracing::warn!(channel = %self.shared.id, %error, "HTML rejected; sending plain text");
                api.send_message(destination, &html_to_plain(text), false, markup)
                    .await
            }
            other => other,
        }
    }

    /// `sendPhoto` of `image` with an HTML `caption` (plain text when Telegram cannot parse it).
    async fn send_photo(
        &self,
        to: &ReplyTarget,
        image: &okbot_core::Image,
        caption: Option<&str>,
        markup: Option<&serde_json::Value>,
    ) -> Result<i64, TelegramError> {
        let destination = destination(to);
        let photo = api::Photo {
            name: if image.name.is_empty() {
                "chart.png"
            } else {
                &image.name
            },
            png: &image.png,
        };
        let api = &self.shared.api;
        match api
            .send_photo(destination, photo, caption, true, markup)
            .await
        {
            Err(error) if error.is_parse_error() => {
                tracing::warn!(channel = %self.shared.id, %error, "HTML caption rejected; sending plain text");
                let plain = caption.map(html_to_plain);
                api.send_photo(destination, photo, plain.as_deref(), false, markup)
                    .await
            }
            other => other,
        }
    }

    /// Replaces the text of a message the bot sent, or its caption when it is a photo.
    ///
    /// The channel does not remember which kind a message is (it works after a restart), so it
    /// tries the likelier edit first (`caption_first` for a prompt with a chart whose text fits a
    /// caption) and, when Telegram answers that the message has no text (or no caption) to edit,
    /// the other. A text too long for a caption becomes a shortened plain caption.
    async fn edit_text(
        &self,
        sent: &SentRef,
        text: &str,
        markup: Option<&serde_json::Value>,
    ) -> Result<(), ChannelError> {
        self.edit_any(sent, text, markup, false).await
    }

    async fn edit_any(
        &self,
        sent: &SentRef,
        text: &str,
        markup: Option<&serde_json::Value>,
        caption_first: bool,
    ) -> Result<(), ChannelError> {
        let result = if caption_first {
            match self.edit_caption_once(sent, text, markup).await {
                Err(error) if error.is_wrong_edit_kind() => {
                    self.edit_text_once(sent, text, markup).await
                }
                other => other,
            }
        } else {
            match self.edit_text_once(sent, text, markup).await {
                Err(error) if error.is_wrong_edit_kind() => {
                    self.edit_caption_once(sent, text, markup).await
                }
                other => other,
            }
        };
        match result {
            Err(error) if error.is_not_modified() => Ok(()),
            other => other.map_err(channel_error),
        }
    }

    /// `editMessageText`, in plain text when Telegram cannot parse the HTML.
    async fn edit_text_once(
        &self,
        sent: &SentRef,
        text: &str,
        markup: Option<&serde_json::Value>,
    ) -> Result<(), TelegramError> {
        let api = &self.shared.api;
        let chat = &sent.target.conversation.room;
        let id = sent.message_id.as_str();
        match api.edit_message(chat, id, text, true, markup).await {
            Err(error) if error.is_parse_error() => {
                api.edit_message(chat, id, &html_to_plain(text), false, markup)
                    .await
            }
            other => other,
        }
    }

    /// `editMessageCaption`, in plain text when Telegram cannot parse the HTML or it is too long.
    async fn edit_caption_once(
        &self,
        sent: &SentRef,
        text: &str,
        markup: Option<&serde_json::Value>,
    ) -> Result<(), TelegramError> {
        let api = &self.shared.api;
        let chat = &sent.target.conversation.room;
        let id = sent.message_id.as_str();
        if !prompt::fits_caption(text) {
            let short = short_caption(&html_to_plain(text));
            return api.edit_caption(chat, id, &short, false, markup).await;
        }
        match api.edit_caption(chat, id, text, true, markup).await {
            Err(error) if error.is_parse_error() => {
                let short = short_caption(&html_to_plain(text));
                api.edit_caption(chat, id, &short, false, markup).await
            }
            other => other,
        }
    }
}

#[async_trait]
impl Channel for TelegramChannel {
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
                "a Telegram channel can only run once",
            ));
        }
        let me = self.shared.api.get_me().await.map_err(channel_error)?;
        tracing::info!(channel = %self.shared.id, bot = %me.username, "Telegram bot ready");
        let _ = self.shared.me.set(Bot {
            id: me.id,
            username: me.username,
        });
        match &self.delivery {
            Delivery::Polling { timeout } => self.poll(*timeout, &shutdown).await,
            Delivery::Webhook { public_url, secret } => {
                self.shared
                    .api
                    .set_webhook(public_url, secret.expose())
                    .await
                    .map_err(channel_error)?;
                tracing::info!(channel = %self.shared.id, url = %public_url, "Telegram webhook registered");
                shutdown.cancelled().await;
                Ok(())
            }
        }
    }

    async fn set_commands(&self, commands: &[okbot_core::CommandInfo]) -> Result<(), ChannelError> {
        // The menu only inserts `/name `: the description shows what to type after it.
        let menu = |language: Option<&str>| -> Vec<(String, String)> {
            commands
                .iter()
                .map(|c| {
                    let description = language
                        .and_then(|l| c.descriptions.iter().find(|(code, _)| code == l))
                        .map_or(c.description.as_str(), |(_, text)| text.as_str());
                    let params: Vec<String> = c
                        .params
                        .iter()
                        .map(|p| {
                            if p.required {
                                format!("<{}>", p.name)
                            } else {
                                format!("[{}]", p.name)
                            }
                        })
                        .collect();
                    let mut d = description.trim().to_string();
                    if !params.is_empty() {
                        d = format!("{} — {d}", params.join(" "));
                    }
                    if d.is_empty() {
                        d.clone_from(&c.name);
                    }
                    (c.name.clone(), d.chars().take(256).collect())
                })
                .collect()
        };
        let api = &self.shared.api;
        api.set_my_commands(&menu(None), None)
            .await
            .map_err(channel_error)?;
        let mut languages: Vec<&str> = commands
            .iter()
            .flat_map(|c| c.descriptions.iter().map(|(code, _)| code.as_str()))
            .collect();
        languages.sort_unstable();
        languages.dedup();
        for language in languages {
            api.set_my_commands(&menu(Some(language)), Some(language))
                .await
                .map_err(channel_error)?;
        }
        Ok(())
    }

    async fn send(
        &self,
        to: &ReplyTarget,
        message: &RenderedMessage,
    ) -> Result<SentRef, ChannelError> {
        let id = self
            .send_text(to, &message.text, None)
            .await
            .map_err(channel_error)?;
        Ok(SentRef {
            target: to.clone(),
            message_id: MessageId::new(id.to_string()),
        })
    }

    /// A private chat's id is the user's id. Telegram still refuses messages to someone who has
    /// not started the bot; sending then fails.
    async fn open_direct(&self, user: &UserId) -> Result<ConversationKey, ChannelError> {
        if user.as_str().parse::<i64>().is_err() {
            return Err(ChannelError::Failed(
                format!("not a Telegram user id: {user}").into(),
            ));
        }
        Ok(ConversationKey::room(self.shared.id.clone(), user.as_str()))
    }

    async fn edit(&self, sent: &SentRef, message: &RenderedMessage) -> Result<(), ChannelError> {
        self.edit_text(sent, &message.text, None).await
    }

    async fn prompt(&self, to: &ReplyTarget, prompt: &Prompt) -> Result<SentRef, ChannelError> {
        // Telegram has no forms: the runtime refuses such prompts or shows them otherwise.
        if prompt.buttons.iter().any(|b| b.form.is_some()) {
            return Err(ChannelError::Unsupported("forms"));
        }
        let open = |id| prompt::Open::new(prompt, to.clone(), id);
        let markup = prompt::markup(prompt);
        let text = prompt::text(prompt);
        let id = match prompt::image(prompt) {
            // One message: the chart, the text as its caption and the buttons.
            Some((_, image)) if !image.png.is_empty() && prompt::fits_caption(&text) => {
                self.send_photo(to, image, Some(&text), Some(&markup)).await
            }
            // Too long for a caption: the chart (captioned with the card's title), then the text
            // with the buttons. A failed photo is an error: the runtime then sends text only.
            Some((card, image)) if !image.png.is_empty() => {
                let title = prompt::heading(card).filter(|t| prompt::fits_caption(t));
                self.send_photo(to, image, title.as_deref(), None)
                    .await
                    .map_err(channel_error)?;
                self.send_text(to, &text, Some(&markup)).await
            }
            _ => self.send_text(to, &text, Some(&markup)).await,
        }
        .map_err(channel_error)?;
        lock(&self.shared.prompts).insert(prompt.id.clone(), open(id));
        Ok(SentRef {
            target: to.clone(),
            message_id: MessageId::new(id.to_string()),
        })
    }

    /// Rebuilds the message from `prompt` alone (it works after a restart): its text and a
    /// keyboard of what is left in it, none when nothing is. A prompt with a chart may sit on a
    /// photo: when its text fits a caption, the caption is edited first and the photo stays; the
    /// text is edited instead when Telegram answers that the message has no caption (the text
    /// went in a message of its own).
    async fn update_prompt(&self, sent: &SentRef, prompt: &Prompt) -> Result<(), ChannelError> {
        let markup = prompt::markup(prompt);
        let text = prompt::text(prompt);
        let caption_first = prompt::image(prompt).is_some() && prompt::fits_caption(&text);
        self.edit_any(sent, &text, Some(&markup), caption_first)
            .await?;
        let mut prompts = lock(&self.shared.prompts);
        match sent.message_id.as_str().parse::<i64>() {
            Ok(message_id) if prompt.is_answerable() => {
                let open = prompt::Open::new(prompt, sent.target.clone(), message_id);
                prompts.insert(prompt.id.clone(), open);
            }
            _ => {
                prompts.remove(&prompt.id);
            }
        }
        Ok(())
    }

    async fn close_prompt(
        &self,
        sent: &SentRef,
        message: &RenderedMessage,
        keep_buttons: bool,
    ) -> Result<(), ChannelError> {
        let markup = {
            let mut prompts = lock(&self.shared.prompts);
            let id = prompts
                .iter()
                .find(|(_, p)| p.message_id.to_string() == sent.message_id.as_str())
                .map(|(id, _)| id.clone());
            match id {
                Some(id) if keep_buttons => prompts.get(&id).map(|p| p.markup.clone()),
                Some(id) => {
                    prompts.remove(&id);
                    None
                }
                None => None,
            }
        };
        let none = prompt::no_buttons();
        self.edit_text(sent, &message.text, Some(markup.as_ref().unwrap_or(&none)))
            .await
    }

    async fn typing(&self, to: &ReplyTarget) -> Result<(), ChannelError> {
        self.shared
            .api
            .typing(&to.conversation.room, to.conversation.thread.as_deref())
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

/// Receives Telegram webhook calls.
struct Webhook {
    shared: Arc<Shared>,
    secret: Secret<String>,
}

#[async_trait]
impl WebhookHandler for Webhook {
    fn path(&self) -> String {
        format!("telegram/{}", self.shared.id.instance())
    }

    async fn handle(&self, request: WebhookRequest) -> WebhookResponse {
        if request.method != "POST" {
            return WebhookResponse::status(405);
        }
        let presented = request.header(SECRET_HEADER).unwrap_or_default();
        if !same_secret(presented.as_bytes(), self.secret.expose().as_bytes()) {
            tracing::warn!(channel = %self.shared.id, "webhook call with a wrong secret");
            return WebhookResponse::status(403);
        }
        match serde_json::from_slice(&request.body) {
            // Delivering only enqueues the event (after resolving file ids), so Telegram gets a
            // fast 200; Telegram retries on errors, and duplicates are dropped by the runtime.
            Ok(raw) => {
                self.shared.deliver(raw).await;
            }
            Err(error) => {
                tracing::debug!(channel = %self.shared.id, %error, "unreadable Telegram update");
            }
        }
        WebhookResponse::json(&serde_json::json!({}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_match_the_platform() {
        let c = TelegramChannel::new(TelegramConfig::polling(
            "telegram:shop".parse().unwrap(),
            Secret::new("1:x".into()),
        ));
        assert_eq!(c.caps().max_message_len, 4096);
        assert_eq!(c.caps().dialect, MarkdownDialect::TelegramHtml);
        assert!(c.caps().typing && c.caps().edit);
        assert!(c.webhook().is_none());
        let ui = c.caps().ui;
        assert!(ui.cards && ui.images && !ui.forms);
    }

    #[test]
    fn long_captions_are_cut() {
        assert_eq!(short_caption("short"), "short");
        let cut = short_caption(&"ab".repeat(600));
        assert_eq!(cut.encode_utf16().count(), 1024);
        assert!(cut.ends_with('…'));
    }

    #[test]
    fn plain_fallback_and_secrets() {
        assert_eq!(
            html_to_plain("<b>Giá</b> &lt;100k&gt; &amp; <a href=\"x\">link</a>"),
            "Giá <100k> & link"
        );
        assert!(valid_secret("abc_DEF-123"));
        assert!(!valid_secret("has space") && !valid_secret(""));
        assert!(same_secret(b"s", b"s") && !same_secret(b"s", b"t"));
    }
}
