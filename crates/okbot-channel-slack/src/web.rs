//! The Slack Web API calls okbot needs.

use std::time::Duration;

use okbot_core::Secret;
use serde_json::{Value, json};

/// The production Web API.
pub const SLACK_API: &str = "https://slack.com/api";

/// A Slack API failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SlackError {
    /// Slack answered `ok: false` with this error code.
    #[error("Slack API error: {0}")]
    Api(String),
    /// Network failure.
    #[error("Slack request failed: {0}")]
    Transport(String),
}

/// A request failure, without the URL (an upload URL carries a signed query).
fn transport(error: reqwest::Error) -> SlackError {
    SlackError::Transport(error.without_url().to_string())
}

/// The most characters of an image's alt text `files.getUploadURLExternal` takes.
const MAX_ALT_TEXT: usize = 1000;

/// Web API client with the app and bot tokens.
#[derive(Clone)]
pub struct SlackWeb {
    http: reqwest::Client,
    base: String,
    app_token: Secret<String>,
    bot_token: Secret<String>,
}

impl std::fmt::Debug for SlackWeb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SlackWeb")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

impl SlackWeb {
    /// A client against `base`.
    pub fn new(base: &str, app_token: Secret<String>, bot_token: Secret<String>) -> Self {
        // reqwest is built without a crypto provider: okbot uses ring only (no-op when installed).
        let _ = rustls::crypto::ring::default_provider().install_default();
        Self {
            http: reqwest::Client::new(),
            base: base.trim_end_matches('/').to_string(),
            app_token,
            bot_token,
        }
    }

    async fn call(
        &self,
        method: &str,
        token: &Secret<String>,
        body: &Value,
    ) -> Result<Value, SlackError> {
        let request = self
            .http
            .post(format!("{}/{method}", self.base))
            .bearer_auth(token.expose())
            .json(body);
        Self::answer(request).await
    }

    /// Sends a Web API request and checks Slack's `ok`.
    async fn answer(request: reqwest::RequestBuilder) -> Result<Value, SlackError> {
        let answer: Value = request
            .timeout(Duration::from_secs(20))
            .send()
            .await
            .map_err(transport)?
            .json()
            .await
            .map_err(transport)?;
        if answer["ok"].as_bool() == Some(true) {
            Ok(answer)
        } else {
            Err(SlackError::Api(
                answer["error"]
                    .as_str()
                    .unwrap_or("unknown_error")
                    .to_string(),
            ))
        }
    }

    /// `apps.connections.open`: a Socket Mode WebSocket URL.
    pub async fn connections_open(&self) -> Result<String, SlackError> {
        let answer = self
            .call("apps.connections.open", &self.app_token, &json!({}))
            .await?;
        answer["url"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| SlackError::Api("no_url".into()))
    }

    /// `auth.test`: the bot's own user id.
    pub async fn auth_test(&self) -> Result<String, SlackError> {
        let answer = self.call("auth.test", &self.bot_token, &json!({})).await?;
        Ok(answer["user_id"].as_str().unwrap_or_default().to_string())
    }

    /// `chat.postMessage`, with Block Kit `blocks` when given; returns the message `ts`.
    pub async fn post_message(
        &self,
        channel: &str,
        thread_ts: Option<&str>,
        text: &str,
        blocks: Option<&Value>,
    ) -> Result<String, SlackError> {
        let mut body =
            json!({ "channel": channel, "text": text, "mrkdwn": true, "unfurl_links": false });
        if let Some(ts) = thread_ts {
            body["thread_ts"] = json!(ts);
        }
        if let Some(blocks) = blocks {
            body["blocks"] = blocks.clone();
        }
        let answer = self
            .call("chat.postMessage", &self.bot_token, &body)
            .await?;
        Ok(answer["ts"].as_str().unwrap_or_default().to_string())
    }

    /// `chat.update`; `blocks` replaces the message's blocks (an empty list removes them).
    pub async fn update(
        &self,
        channel: &str,
        ts: &str,
        text: &str,
        blocks: Option<&Value>,
    ) -> Result<(), SlackError> {
        let mut body = json!({ "channel": channel, "ts": ts, "text": text });
        if let Some(blocks) = blocks {
            body["blocks"] = blocks.clone();
        }
        self.call("chat.update", &self.bot_token, &body)
            .await
            .map(drop)
    }

    /// `views.open`: shows a modal for an interaction's `trigger_id` (valid for 3 seconds).
    pub async fn views_open(&self, trigger_id: &str, view: &Value) -> Result<(), SlackError> {
        let body = json!({ "trigger_id": trigger_id, "view": view });
        self.call("views.open", &self.bot_token, &body)
            .await
            .map(drop)
    }

    /// `chat.postEphemeral`: a message only `user` sees.
    pub async fn post_ephemeral(
        &self,
        channel: &str,
        user: &str,
        thread_ts: Option<&str>,
        text: &str,
    ) -> Result<(), SlackError> {
        let mut body = json!({ "channel": channel, "user": user, "text": text });
        if let Some(ts) = thread_ts {
            body["thread_ts"] = json!(ts);
        }
        self.call("chat.postEphemeral", &self.bot_token, &body)
            .await
            .map(drop)
    }

    /// Uploads a file and shares it in `channel` (in the thread `thread_ts`) as its own message:
    /// `files.getUploadURLExternal`, the bytes to the returned URL, then
    /// `files.completeUploadExternal`. Needs the `files:write` scope.
    pub async fn upload_file(
        &self,
        channel: &str,
        thread_ts: Option<&str>,
        file: &Upload<'_>,
    ) -> Result<(), SlackError> {
        let alt: String = file.alt.chars().take(MAX_ALT_TEXT).collect();
        let length = file.bytes.len().to_string();
        // A form, as Slack's own SDKs send it; `length` must be a number in the form.
        let ticket = Self::answer(
            self.http
                .post(format!("{}/files.getUploadURLExternal", self.base))
                .bearer_auth(self.bot_token.expose())
                .form(&[
                    ("filename", file.name),
                    ("length", length.as_str()),
                    ("alt_txt", alt.as_str()),
                ]),
        )
        .await?;
        let (Some(upload_url), Some(file_id)) =
            (ticket["upload_url"].as_str(), ticket["file_id"].as_str())
        else {
            return Err(SlackError::Api("no_upload_url".into()));
        };
        // The upload URL is signed: it gets no token, and it is never logged.
        let part = reqwest::multipart::Part::bytes(file.bytes.to_vec())
            .file_name(file.name.to_string())
            .mime_str(file.mime)
            .map_err(transport)?;
        let status = self
            .http
            .post(upload_url)
            .timeout(Duration::from_secs(60))
            .multipart(reqwest::multipart::Form::new().part("file", part))
            .send()
            .await
            .map_err(transport)?
            .status();
        if !status.is_success() {
            return Err(SlackError::Transport(format!(
                "file upload answered HTTP {status}"
            )));
        }
        let mut body = json!({
            "files": [{ "id": file_id, "title": file.title }],
            "channel_id": channel,
        });
        if let Some(ts) = thread_ts {
            body["thread_ts"] = json!(ts);
        }
        self.call("files.completeUploadExternal", &self.bot_token, &body)
            .await
            .map(drop)
    }
}

/// A file to share with [`SlackWeb::upload_file`].
#[derive(Debug, Clone, Copy)]
pub struct Upload<'a> {
    /// File name (`chart.png`).
    pub name: &'a str,
    /// Title shown above the file.
    pub title: &'a str,
    /// Alt text for an image (cut to 1,000 characters).
    pub alt: &'a str,
    /// Media type (`image/png`).
    pub mime: &'a str,
    /// The contents.
    pub bytes: &'a [u8],
}
