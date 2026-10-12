//! Platform-neutral HTTP types for channels that receive events by webhook.
//!
//! The runtime owns the HTTP server and mounts each channel's [`WebhookHandler`] at its path, so
//! channel crates do not depend on a web framework.

use crate::async_trait;

/// An HTTP request delivered to a webhook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookRequest {
    /// Upper-case method (`POST`).
    pub method: String,
    /// Path relative to the handler's mount point (usually empty).
    pub path: String,
    /// Headers with lower-case names.
    pub headers: Vec<(String, String)>,
    /// Raw body.
    pub body: Vec<u8>,
}

impl WebhookRequest {
    /// The first value of a header (`name` in lower case).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// The response to a webhook request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookResponse {
    /// HTTP status.
    pub status: u16,
    /// `Content-Type` of the body.
    pub content_type: &'static str,
    /// Body.
    pub body: Vec<u8>,
}

impl WebhookResponse {
    /// `200` with a JSON body.
    pub fn json(body: &serde_json::Value) -> Self {
        Self {
            status: 200,
            content_type: "application/json",
            body: body.to_string().into_bytes(),
        }
    }

    /// An empty response with `status`.
    pub fn status(status: u16) -> Self {
        Self {
            status,
            content_type: "text/plain",
            body: Vec::new(),
        }
    }
}

/// Receives webhook requests for one channel.
///
/// Implementations authenticate the request, hand events to the runtime and return quickly:
/// platforms time out slow webhooks, and agent turns are slow.
#[async_trait]
pub trait WebhookHandler: Send + Sync {
    /// Mount path, without the bot prefix (for example `zalo/shop`).
    fn path(&self) -> String;

    /// Handles one request.
    async fn handle(&self, request: WebhookRequest) -> WebhookResponse;
}
