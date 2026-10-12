//! Receiving Chat events from a Cloud Pub/Sub pull subscription.

use base64::Engine as _;
use serde_json::{Value, json};

use crate::api::{ApiError, call};
use crate::auth::TokenSource;

/// The production Pub/Sub API.
pub const PUBSUB_API: &str = "https://pubsub.googleapis.com";

/// A pull subscription (`projects/<p>/subscriptions/<s>`).
#[derive(Debug, Clone)]
pub struct Subscription {
    pub(crate) http: reqwest::Client,
    pub(crate) base: String,
    pub(crate) tokens: TokenSource,
    pub(crate) name: String,
}

impl Subscription {
    /// Pulls up to 10 events. Returns `(ack_id, event JSON)` pairs.
    pub async fn pull(&self) -> Result<Vec<(String, Value)>, ApiError> {
        let url = format!("{}/v1/{}:pull", self.base, self.name);
        let body = json!({ "maxMessages": 10 });
        let answer = call(&self.http, &self.tokens, reqwest::Method::POST, &url, &body).await?;
        let mut out = Vec::new();
        for received in answer["receivedMessages"].as_array().into_iter().flatten() {
            let ack = received["ackId"].as_str().unwrap_or_default().to_string();
            let data = received["message"]["data"].as_str().unwrap_or_default();
            let event = base64::engine::general_purpose::STANDARD
                .decode(data)
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .unwrap_or(Value::Null);
            out.push((ack, event));
        }
        Ok(out)
    }

    /// Acknowledges handled messages.
    pub async fn ack(&self, ack_ids: &[String]) -> Result<(), ApiError> {
        if ack_ids.is_empty() {
            return Ok(());
        }
        let url = format!("{}/v1/{}:acknowledge", self.base, self.name);
        call(
            &self.http,
            &self.tokens,
            reqwest::Method::POST,
            &url,
            &json!({ "ackIds": ack_ids }),
        )
        .await
        .map(drop)
    }
}
