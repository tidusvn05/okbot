//! A small client for the Zalo Bot API (`https://bot-api.zaloplatforms.com/bot<TOKEN>/<method>`).
//!
//! The token is part of every URL, so this client never logs URLs and strips them from errors.

use std::time::Duration;

use okbot_core::Secret;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

/// The production API base.
pub const DEFAULT_API_BASE: &str = "https://bot-api.zaloplatforms.com";

/// A Zalo Bot API failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ApiError {
    /// The token was rejected (401).
    #[error("Zalo rejected the bot token (401): {0}")]
    Unauthorized(String),
    /// Over the API quota (429).
    #[error("Zalo API quota exceeded (429): {0}")]
    RateLimited(String),
    /// The API answered `ok: false` with another code.
    #[error("Zalo API error {code}: {description}")]
    Api {
        /// `error_code`.
        code: i64,
        /// `description`.
        description: String,
    },
    /// The request failed before an API answer (network, timeout). The URL is never included.
    #[error("Zalo API request failed: {0}")]
    Transport(String),
    /// The answer could not be read.
    #[error("unexpected Zalo API answer: {0}")]
    Protocol(String),
}

impl ApiError {
    /// Whether retrying later may succeed.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::RateLimited(_) | Self::Transport(_) => true,
            Self::Api { code, .. } => *code == 408 || *code >= 500,
            _ => false,
        }
    }
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    result: Value,
    #[serde(default)]
    description: Option<String>,
    #[serde(default, alias = "errorCode")]
    error_code: Option<i64>,
}

/// Bot information from `getMe`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct BotInfo {
    /// Bot id.
    pub id: String,
    /// Account name.
    #[serde(default)]
    pub account_name: String,
    /// `BASIC` or a paid plan.
    #[serde(default)]
    pub account_type: String,
    /// Whether the bot may be added to groups.
    #[serde(default)]
    pub can_join_groups: bool,
}

/// The webhook check returned by `setWebhook` and `testWebhook`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct WebhookCheck {
    /// The URL answered 2xx.
    #[serde(default)]
    pub ok: bool,
    /// Outcome code (`webhook.ok`, `webhook.http.403`, …).
    #[serde(default)]
    pub outcome: String,
    /// Zalo's hint.
    #[serde(default)]
    pub hint: String,
}

/// The Zalo Bot API for one bot token.
#[derive(Clone)]
pub struct ZaloApi {
    http: reqwest::Client,
    base: String,
    token: Secret<String>,
}

impl std::fmt::Debug for ZaloApi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ZaloApi")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

impl ZaloApi {
    /// A client for `token` against `base` (normally [`DEFAULT_API_BASE`]).
    pub fn new(base: &str, token: Secret<String>) -> Self {
        // reqwest is built without a crypto provider: okbot uses ring only (no-op when installed).
        let _ = rustls::crypto::ring::default_provider().install_default();
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .user_agent(concat!("okbot/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_default();
        Self {
            http,
            base: base.trim_end_matches('/').to_string(),
            token,
        }
    }

    /// Calls `method` with a JSON body, waiting at most `timeout`.
    pub async fn call_raw(
        &self,
        method: &str,
        body: &Value,
        timeout: Duration,
    ) -> Result<Value, ApiError> {
        let url = format!("{}/bot{}/{method}", self.base, self.token.expose());
        let response = self
            .http
            .post(url)
            .timeout(timeout)
            .json(body)
            .send()
            .await
            // `without_url` keeps the token out of the message.
            .map_err(|e| ApiError::Transport(e.without_url().to_string()))?;
        let status = response.status().as_u16();
        let text = response
            .text()
            .await
            .map_err(|e| ApiError::Transport(e.without_url().to_string()))?;
        let envelope: Envelope = serde_json::from_str(&text)
            .map_err(|e| ApiError::Protocol(format!("HTTP {status}: {e}")))?;
        if envelope.ok {
            return Ok(envelope.result);
        }
        let description = envelope.description.unwrap_or_default();
        Err(match envelope.error_code.unwrap_or(i64::from(status)) {
            401 => ApiError::Unauthorized(description),
            429 => ApiError::RateLimited(description),
            code => ApiError::Api { code, description },
        })
    }

    async fn call<T: DeserializeOwned>(&self, method: &str, body: &Value) -> Result<T, ApiError> {
        let value = self.call_raw(method, body, Duration::from_secs(30)).await?;
        serde_json::from_value(value).map_err(|e| ApiError::Protocol(format!("{method}: {e}")))
    }

    /// `getMe`: checks the token.
    pub async fn get_me(&self) -> Result<BotInfo, ApiError> {
        self.call("getMe", &json!({})).await
    }

    /// `getUpdates`: long-polls for new events. Returns zero or more raw updates.
    pub async fn get_updates(&self, timeout: Duration) -> Result<Vec<Value>, ApiError> {
        let body = json!({ "timeout": timeout.as_secs().to_string() });
        let result = self
            .call_raw("getUpdates", &body, timeout + Duration::from_secs(10))
            .await;
        match result {
            // No event within the timeout: an empty result, or a 408 on some API versions.
            Err(ApiError::Api { code: 408, .. }) | Ok(Value::Null) => Ok(Vec::new()),
            Ok(Value::Object(map)) if map.is_empty() => Ok(Vec::new()),
            Err(e) => Err(e),
            Ok(Value::Array(items)) => Ok(items),
            Ok(other) => Ok(vec![other]),
        }
    }

    /// `setWebhook`: registers `url` with `secret`; returns Zalo's immediate check, if any.
    pub async fn set_webhook(
        &self,
        url: &str,
        secret: &Secret<String>,
    ) -> Result<Option<WebhookCheck>, ApiError> {
        let result: Value = self
            .call(
                "setWebhook",
                &json!({ "url": url, "secret_token": secret.expose() }),
            )
            .await?;
        Ok(serde_json::from_value(result["verification"].clone()).ok())
    }

    /// `deleteWebhook`: switches back to `getUpdates`.
    pub async fn delete_webhook(&self) -> Result<(), ApiError> {
        self.call::<Value>("deleteWebhook", &json!({}))
            .await
            .map(drop)
    }

    /// `getWebhookInfo`: the registered URL (empty when none).
    pub async fn webhook_url(&self) -> Result<String, ApiError> {
        let info: Value = self.call("getWebhookInfo", &json!({})).await?;
        Ok(info["url"].as_str().unwrap_or_default().to_string())
    }

    /// `testWebhook` (rate-limited by Zalo: call it from `doctor` only).
    pub async fn test_webhook(&self) -> Result<WebhookCheck, ApiError> {
        self.call("testWebhook", &json!({})).await
    }

    /// `sendMessage` with Markdown parsing. Returns the message id.
    pub async fn send_message(&self, chat_id: &str, text: &str) -> Result<String, ApiError> {
        let result: Value = self
            .call(
                "sendMessage",
                &json!({ "chat_id": chat_id, "text": text, "parse_mode": "markdown" }),
            )
            .await?;
        Ok(result["message_id"]
            .as_str()
            .unwrap_or_default()
            .to_string())
    }

    /// `sendChatAction` with `typing`.
    pub async fn typing(&self, chat_id: &str) -> Result<(), ApiError> {
        self.call::<Value>(
            "sendChatAction",
            &json!({ "chat_id": chat_id, "action": "typing" }),
        )
        .await
        .map(drop)
    }
}
