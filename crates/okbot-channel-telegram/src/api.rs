//! The Telegram Bot API calls okbot needs.
//!
//! The token is part of every request URL (`/bot<token>/<method>`), so errors are built from
//! reqwest errors with [`reqwest::Error::without_url`] and the client's `Debug` hides the token.

use std::time::Duration;

use okbot_core::Secret;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

/// The production Bot API.
pub const API_BASE: &str = "https://api.telegram.org";

/// A Bot API failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TelegramError {
    /// Telegram answered `ok: false`.
    #[error("Telegram API error {code}: {description}")]
    Api {
        /// `error_code` (an HTTP status).
        code: u16,
        /// `description`.
        description: String,
        /// `parameters.retry_after`, on 429.
        retry_after: Option<u64>,
    },
    /// Network failure or an unreadable answer.
    #[error("Telegram request failed: {0}")]
    Transport(String),
}

impl TelegramError {
    /// The API error code, if Telegram answered.
    pub fn code(&self) -> Option<u16> {
        match self {
            Self::Api { code, .. } => Some(*code),
            Self::Transport(_) => None,
        }
    }

    /// The token was rejected (401) or does not name a bot (404).
    pub fn is_unauthorized(&self) -> bool {
        matches!(self.code(), Some(401 | 404))
    }

    /// The HTML could not be parsed (`can't parse entities`).
    pub(crate) fn is_parse_error(&self) -> bool {
        matches!(self, Self::Api { code: 400, description, .. } if description.contains("can't parse entities"))
    }

    /// An edit of the wrong kind: `editMessageText` on a photo ("there is no text in the message
    /// to edit") or `editMessageCaption` on a text message ("there is no caption ...").
    pub(crate) fn is_wrong_edit_kind(&self) -> bool {
        matches!(self, Self::Api { code: 400, description, .. }
            if description.contains("no text in the message")
                || description.contains("no caption in the message"))
    }

    /// An edit with the same text as before.
    pub(crate) fn is_not_modified(&self) -> bool {
        matches!(self, Self::Api { code: 400, description, .. } if description.contains("message is not modified"))
    }
}

#[derive(Deserialize)]
struct Answer {
    ok: bool,
    #[serde(default)]
    result: Value,
    #[serde(default)]
    error_code: u16,
    #[serde(default)]
    description: String,
    #[serde(default)]
    parameters: Option<Parameters>,
}

#[derive(Deserialize)]
struct Parameters {
    retry_after: Option<u64>,
}

/// `getMe`.
#[derive(Debug, Clone, Deserialize)]
pub struct Me {
    /// The bot's user id.
    pub id: i64,
    /// `@username` without the `@`.
    #[serde(default)]
    pub username: String,
    /// Display name.
    #[serde(default)]
    pub first_name: String,
    /// Whether privacy mode is off (the bot sees every group message).
    #[serde(default)]
    pub can_read_all_group_messages: bool,
}

/// `getWebhookInfo`.
#[derive(Debug, Clone, Deserialize)]
pub struct WebhookInfo {
    /// The registered URL, empty when none.
    #[serde(default)]
    pub url: String,
    /// Updates waiting to be delivered.
    #[serde(default)]
    pub pending_update_count: u64,
    /// The last delivery error, if any.
    #[serde(default)]
    pub last_error_message: Option<String>,
}

#[derive(Deserialize)]
struct Sent {
    message_id: i64,
}

#[derive(Deserialize)]
struct File {
    #[serde(default)]
    file_path: Option<String>,
}

/// Where a message goes.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Destination<'a> {
    pub(crate) chat_id: &'a str,
    /// Forum topic.
    pub(crate) thread: Option<&'a str>,
    /// Quote this message.
    pub(crate) reply_to: Option<&'a str>,
}

/// A Bot API client.
#[derive(Clone)]
pub struct TelegramApi {
    http: reqwest::Client,
    base: String,
    token: Secret<String>,
}

impl std::fmt::Debug for TelegramApi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelegramApi")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

/// A picture to upload.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Photo<'a> {
    /// The file name (`chart.png`).
    pub(crate) name: &'a str,
    /// PNG data.
    pub(crate) png: &'a [u8],
}

/// The fields that address a message to `to`: chat, forum topic and the quoted message.
fn destination_fields(to: Destination<'_>) -> serde_json::Map<String, Value> {
    let mut fields = serde_json::Map::new();
    fields.insert("chat_id".into(), chat_id(to.chat_id));
    if let Some(thread) = to.thread.and_then(|t| t.parse::<i64>().ok()) {
        fields.insert("message_thread_id".into(), json!(thread));
    }
    if let Some(id) = to.reply_to.and_then(|m| m.parse::<i64>().ok()) {
        fields.insert(
            "reply_parameters".into(),
            json!({ "message_id": id, "allow_sending_without_reply": true }),
        );
    }
    fields
}

/// A message id as the number the API takes.
fn message_id_number(message_id: &str) -> Result<i64, TelegramError> {
    message_id.parse::<i64>().map_err(|_| TelegramError::Api {
        code: 400,
        description: format!("not a Telegram message id: {message_id}"),
        retry_after: None,
    })
}

/// Chat ids are integers; okbot keeps them as text. Numbers are sent as numbers.
fn chat_id(id: &str) -> Value {
    id.parse::<i64>().map_or_else(|_| json!(id), |n| json!(n))
}

impl TelegramApi {
    /// A client for `token` against `base` (the production API is [`API_BASE`]).
    pub fn new(base: &str, token: Secret<String>) -> Self {
        // reqwest is built without a crypto provider: okbot uses ring only (no-op when installed).
        let _ = rustls::crypto::ring::default_provider().install_default();
        Self {
            http: reqwest::Client::new(),
            base: base.trim_end_matches('/').to_string(),
            token,
        }
    }

    fn url(&self, method: &str) -> String {
        format!("{}/bot{}/{method}", self.base, self.token.expose())
    }

    async fn call<T: DeserializeOwned>(
        &self,
        method: &str,
        body: &Value,
        timeout: Duration,
    ) -> Result<T, TelegramError> {
        let request = self.http.post(self.url(method)).timeout(timeout).json(body);
        Self::answer(method, request).await
    }

    /// A call with a `multipart/form-data` body (file uploads).
    async fn call_multipart<T: DeserializeOwned>(
        &self,
        method: &str,
        form: reqwest::multipart::Form,
        timeout: Duration,
    ) -> Result<T, TelegramError> {
        let request = self
            .http
            .post(self.url(method))
            .timeout(timeout)
            .multipart(form);
        Self::answer(method, request).await
    }

    /// Sends `request` and reads Telegram's answer. Errors never carry the URL (it holds the token).
    async fn answer<T: DeserializeOwned>(
        method: &str,
        request: reqwest::RequestBuilder,
    ) -> Result<T, TelegramError> {
        let answer: Answer = request
            .send()
            .await
            .map_err(|e| TelegramError::Transport(e.without_url().to_string()))?
            .json()
            .await
            .map_err(|e| TelegramError::Transport(e.without_url().to_string()))?;
        if !answer.ok {
            return Err(TelegramError::Api {
                code: answer.error_code,
                description: answer.description,
                retry_after: answer.parameters.and_then(|p| p.retry_after),
            });
        }
        serde_json::from_value(answer.result)
            .map_err(|e| TelegramError::Transport(format!("unexpected answer to {method}: {e}")))
    }

    const SHORT: Duration = Duration::from_secs(20);
    /// For uploads.
    const UPLOAD: Duration = Duration::from_secs(60);

    /// `getMe` (checks the token).
    pub async fn get_me(&self) -> Result<Me, TelegramError> {
        self.call("getMe", &json!({}), Self::SHORT).await
    }

    /// `getWebhookInfo`.
    pub async fn webhook_info(&self) -> Result<WebhookInfo, TelegramError> {
        self.call("getWebhookInfo", &json!({}), Self::SHORT).await
    }

    /// `getUpdates`, long polling for `timeout` from `offset`. Raw updates, parsed one by one so
    /// that one unknown shape does not block the queue.
    pub(crate) async fn get_updates(
        &self,
        offset: i64,
        timeout: Duration,
    ) -> Result<Vec<Value>, TelegramError> {
        let body = json!({
            "offset": offset,
            "timeout": timeout.as_secs(),
            "allowed_updates": ["message", "callback_query"],
        });
        self.call("getUpdates", &body, timeout + Duration::from_secs(10))
            .await
    }

    /// `deleteWebhook` (keeps pending updates).
    pub(crate) async fn delete_webhook(&self) -> Result<(), TelegramError> {
        self.call::<Value>("deleteWebhook", &json!({}), Self::SHORT)
            .await
            .map(drop)
    }

    /// `setMyCommands`: the command menu, as `(command, description)`.
    /// `language_code`: the menu for users whose app is in that language (none: everyone else).
    pub(crate) async fn set_my_commands(
        &self,
        commands: &[(String, String)],
        language_code: Option<&str>,
    ) -> Result<(), TelegramError> {
        let list: Vec<Value> = commands
            .iter()
            .map(|(c, d)| json!({"command": c, "description": d}))
            .collect();
        let mut body = json!({ "commands": list });
        if let Some(code) = language_code {
            body["language_code"] = json!(code);
        }
        self.call::<Value>("setMyCommands", &body, Self::SHORT)
            .await
            .map(drop)
    }

    /// `setWebhook` for message updates, authenticated by `secret`.
    pub(crate) async fn set_webhook(&self, url: &str, secret: &str) -> Result<(), TelegramError> {
        let body = json!({
            "url": url,
            "secret_token": secret,
            "allowed_updates": ["message", "callback_query"],
        });
        self.call::<Value>("setWebhook", &body, Self::SHORT)
            .await
            .map(drop)
    }

    /// `sendMessage`; `html` selects `parse_mode = "HTML"`, `markup` adds buttons. Returns the
    /// message id.
    pub(crate) async fn send_message(
        &self,
        to: Destination<'_>,
        text: &str,
        html: bool,
        markup: Option<&Value>,
    ) -> Result<i64, TelegramError> {
        let mut body = Value::Object(destination_fields(to));
        body["text"] = json!(text);
        body["link_preview_options"] = json!({ "is_disabled": true });
        if html {
            body["parse_mode"] = json!("HTML");
        }
        if let Some(markup) = markup {
            body["reply_markup"] = markup.clone();
        }
        let sent: Sent = self.call("sendMessage", &body, Self::SHORT).await?;
        Ok(sent.message_id)
    }

    /// `sendPhoto` with the picture uploaded as multipart (`photo`, a PNG named `name`), an
    /// optional `caption` (`html` selects `parse_mode = "HTML"`) and `markup` buttons. Returns the
    /// message id.
    pub(crate) async fn send_photo(
        &self,
        to: Destination<'_>,
        photo: Photo<'_>,
        caption: Option<&str>,
        html: bool,
        markup: Option<&Value>,
    ) -> Result<i64, TelegramError> {
        let part = reqwest::multipart::Part::bytes(photo.png.to_vec())
            .file_name(photo.name.to_string())
            .mime_str("image/png")
            .map_err(|e| TelegramError::Transport(e.without_url().to_string()))?;
        let mut form = reqwest::multipart::Form::new().part("photo", part);
        for (key, value) in destination_fields(to) {
            // Multipart fields are text: numbers as digits, objects as JSON.
            let value = match value {
                Value::String(s) => s,
                other => other.to_string(),
            };
            form = form.text(key, value);
        }
        if let Some(caption) = caption {
            form = form.text("caption", caption.to_string());
            if html {
                form = form.text("parse_mode", "HTML");
            }
        }
        if let Some(markup) = markup {
            form = form.text("reply_markup", markup.to_string());
        }
        let sent: Sent = self.call_multipart("sendPhoto", form, Self::UPLOAD).await?;
        Ok(sent.message_id)
    }

    /// `editMessageCaption` of a photo message; `markup` as in [`Self::edit_message`].
    pub(crate) async fn edit_caption(
        &self,
        chat: &str,
        message_id: &str,
        caption: &str,
        html: bool,
        markup: Option<&Value>,
    ) -> Result<(), TelegramError> {
        let mut body = json!({
            "chat_id": chat_id(chat),
            "message_id": message_id_number(message_id)?,
            "caption": caption,
        });
        if html {
            body["parse_mode"] = json!("HTML");
        }
        if let Some(markup) = markup {
            body["reply_markup"] = markup.clone();
        }
        self.call::<Value>("editMessageCaption", &body, Self::SHORT)
            .await
            .map(drop)
    }

    /// `editMessageText`; `markup` replaces the buttons (an empty keyboard removes them, `None`
    /// leaves the message without buttons as Telegram does on any text edit).
    pub(crate) async fn edit_message(
        &self,
        chat: &str,
        message_id: &str,
        text: &str,
        html: bool,
        markup: Option<&Value>,
    ) -> Result<(), TelegramError> {
        let mut body = json!({
            "chat_id": chat_id(chat),
            "message_id": message_id_number(message_id)?,
            "text": text,
            "link_preview_options": { "is_disabled": true },
        });
        if html {
            body["parse_mode"] = json!("HTML");
        }
        if let Some(markup) = markup {
            body["reply_markup"] = markup.clone();
        }
        self.call::<Value>("editMessageText", &body, Self::SHORT)
            .await
            .map(drop)
    }

    /// `answerCallbackQuery`: stops the button's spinner; `text` shows as a notice to the presser.
    pub(crate) async fn answer_callback(
        &self,
        id: &str,
        text: Option<&str>,
    ) -> Result<(), TelegramError> {
        let mut body = json!({ "callback_query_id": id });
        if let Some(text) = text {
            body["text"] = json!(text);
            body["show_alert"] = json!(true);
        }
        self.call::<Value>("answerCallbackQuery", &body, Self::SHORT)
            .await
            .map(drop)
    }

    /// `sendChatAction` with `typing`.
    pub(crate) async fn typing(
        &self,
        chat: &str,
        thread: Option<&str>,
    ) -> Result<(), TelegramError> {
        let mut body = json!({ "chat_id": chat_id(chat), "action": "typing" });
        if let Some(thread) = thread.and_then(|t| t.parse::<i64>().ok()) {
            body["message_thread_id"] = json!(thread);
        }
        self.call::<Value>("sendChatAction", &body, Self::SHORT)
            .await
            .map(drop)
    }

    /// `getFile`: the download URL of a file (valid for at least an hour, up to 20 MB).
    ///
    /// The URL contains the bot token. okbot only hands it to its own download code, which never
    /// logs URLs; agents see attachment numbers, never URLs.
    pub(crate) async fn file_url(&self, file_id: &str) -> Result<String, TelegramError> {
        let file: File = self
            .call("getFile", &json!({ "file_id": file_id }), Self::SHORT)
            .await?;
        let path = file.file_path.ok_or_else(|| TelegramError::Api {
            code: 400,
            description: "the file is too big to download".into(),
            retry_after: None,
        })?;
        Ok(format!(
            "{}/file/bot{}/{path}",
            self.base,
            self.token.expose()
        ))
    }
}
