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
        let answer: Value = self
            .http
            .post(format!("{}/{method}", self.base))
            .bearer_auth(token.expose())
            .timeout(Duration::from_secs(20))
            .json(body)
            .send()
            .await
            .map_err(|e| SlackError::Transport(e.to_string()))?
            .json()
            .await
            .map_err(|e| SlackError::Transport(e.to_string()))?;
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

    /// `chat.postMessage`; returns the message `ts`.
    pub async fn post_message(
        &self,
        channel: &str,
        thread_ts: Option<&str>,
        text: &str,
    ) -> Result<String, SlackError> {
        let mut body =
            json!({ "channel": channel, "text": text, "mrkdwn": true, "unfurl_links": false });
        if let Some(ts) = thread_ts {
            body["thread_ts"] = json!(ts);
        }
        let answer = self
            .call("chat.postMessage", &self.bot_token, &body)
            .await?;
        Ok(answer["ts"].as_str().unwrap_or_default().to_string())
    }

    /// `chat.update`.
    pub async fn update(&self, channel: &str, ts: &str, text: &str) -> Result<(), SlackError> {
        self.call(
            "chat.update",
            &self.bot_token,
            &json!({ "channel": channel, "ts": ts, "text": text }),
        )
        .await
        .map(drop)
    }
}
