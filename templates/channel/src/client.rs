//! The platform API client. Replace this in-memory stand-in with your platform's HTTP or
//! WebSocket client; keep it free of okbot types so it can be tested against a fake server.

use std::collections::VecDeque;
use std::sync::Mutex;

/// A platform error. Map these to [`okbot_core::ChannelError`] in `lib.rs`.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// The token was rejected: okbot stops the channel instead of retrying.
    #[error("unauthorized")]
    Unauthorized,
    /// A network or server failure worth retrying.
    #[error("transport: {0}")]
    Transport(String),
}

/// A message as the platform delivers it.
#[derive(Debug, Clone)]
pub struct PlatformMessage {
    /// Platform's unique id (used for deduplication).
    pub id: String,
    /// Where it was posted.
    pub room: String,
    /// Sender id.
    pub user: String,
    /// Sender display name.
    pub user_name: String,
    /// Text.
    pub text: String,
    /// Whether the room is a direct message.
    pub direct: bool,
}

/// TODO(platform): the real client.
#[derive(Debug, Default)]
pub struct Client {
    inbox: Mutex<VecDeque<PlatformMessage>>,
    outbox: Mutex<Vec<(String, String)>>,
}

impl Client {
    /// Queues a message as if the platform delivered it (tests only in this stand-in).
    pub fn push(&self, message: PlatformMessage) {
        self.inbox
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(message);
    }

    /// Fetches new messages (a long poll, a WebSocket frame, …).
    pub async fn next_messages(&self) -> Result<Vec<PlatformMessage>, ClientError> {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        Ok(self
            .inbox
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain(..)
            .collect())
    }

    /// Posts `text` to `room`; returns the new message id.
    pub async fn post(&self, room: &str, text: &str) -> Result<String, ClientError> {
        tokio::task::yield_now().await; // stands in for the network round trip
        let mut outbox = self
            .outbox
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        outbox.push((room.to_string(), text.to_string()));
        Ok(format!("msg-{}", outbox.len()))
    }

    /// Messages posted so far (tests only in this stand-in).
    pub fn posted(&self) -> Vec<(String, String)> {
        self.outbox
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}
