//! The Google Chat REST API: posting and editing the app's messages.

use std::time::Duration;

use serde_json::{Value, json};

use crate::auth::TokenSource;

/// The production Chat API.
pub const CHAT_API: &str = "https://chat.googleapis.com";

/// A Chat or Pub/Sub API failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ApiError {
    /// Authentication failed.
    #[error(transparent)]
    Auth(#[from] crate::auth::AuthError),
    /// The API refused the request.
    #[error("Google API {status}: {message}")]
    Http {
        /// HTTP status.
        status: u16,
        /// Google's message.
        message: String,
    },
    /// Network failure.
    #[error("Google API request failed: {0}")]
    Transport(String),
}

impl ApiError {
    /// Whether retrying later may help.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Http { status, .. } => *status == 429 || *status >= 500,
            Self::Transport(_) | Self::Auth(crate::auth::AuthError::Transport(_)) => true,
            Self::Auth(_) => false,
        }
    }
}

/// Sends an authenticated JSON request and returns the JSON answer.
pub(crate) async fn call(
    http: &reqwest::Client,
    tokens: &TokenSource,
    method: reqwest::Method,
    url: &str,
    body: &Value,
) -> Result<Value, ApiError> {
    let token = tokens.token().await?;
    let response = http
        .request(method, url)
        .bearer_auth(token.expose())
        .timeout(Duration::from_secs(30))
        .json(body)
        .send()
        .await
        .map_err(|e| ApiError::Transport(e.to_string()))?;
    let status = response.status().as_u16();
    let value: Value = response.json().await.unwrap_or(Value::Null);
    if (200..300).contains(&status) {
        Ok(value)
    } else {
        Err(ApiError::Http {
            status,
            message: value["error"]["message"]
                .as_str()
                .unwrap_or("no details")
                .to_string(),
        })
    }
}

/// The Chat API for one app.
#[derive(Debug, Clone)]
pub struct ChatApi {
    pub(crate) http: reqwest::Client,
    pub(crate) base: String,
    pub(crate) tokens: TokenSource,
}

impl ChatApi {
    /// Posts `text` (and `cards`, a `cardsV2` list, when given) in `space`, in `thread` when
    /// given. Returns the message name.
    pub async fn create_message(
        &self,
        space: &str,
        thread: Option<&str>,
        text: &str,
        cards: Option<&Value>,
    ) -> Result<String, ApiError> {
        let mut body = json!({ "text": text });
        if let Some(cards) = cards {
            body["cardsV2"] = cards.clone();
        }
        let mut url = format!("{}/v1/{space}/messages", self.base);
        if let Some(thread) = thread {
            body["thread"] = json!({ "name": thread });
            url.push_str("?messageReplyOption=REPLY_MESSAGE_FALLBACK_TO_NEW_THREAD");
        }
        let message = call(&self.http, &self.tokens, reqwest::Method::POST, &url, &body).await?;
        Ok(message["name"].as_str().unwrap_or_default().to_string())
    }

    /// Replaces the text of the app's message `name`, and its cards when `cards` is given (an
    /// empty list removes them).
    pub async fn update_message(
        &self,
        name: &str,
        text: &str,
        cards: Option<&Value>,
    ) -> Result<(), ApiError> {
        let (mask, body) = match cards {
            Some(cards) => ("text,cardsV2", json!({ "text": text, "cardsV2": cards })),
            None => ("text", json!({ "text": text })),
        };
        let url = format!("{}/v1/{name}?updateMask={mask}", self.base);
        call(
            &self.http,
            &self.tokens,
            reqwest::Method::PATCH,
            &url,
            &body,
        )
        .await
        .map(drop)
    }
}
