//! Test doubles and fixtures for okbot crates and third-party adapters.
//!
//! - [`ManualClock`]: a clock tests move by hand.
//! - [`conformance`]: behaviour every implementation of a port must show.
//! - Small builders for common values ([`conversation`], [`user_message`]).

pub mod conformance;

use std::sync::Mutex;
use std::time::Duration;

use jiff::Timestamp;
use okbot_core::{Clock, ConversationKey, MessageRole, NewMessage, UserId};

/// A clock that only moves when told to. Starts at 2026-01-01T00:00:00Z.
#[derive(Debug)]
pub struct ManualClock(Mutex<Timestamp>);

impl Default for ManualClock {
    fn default() -> Self {
        Self(Mutex::new(
            "2026-01-01T00:00:00Z"
                .parse()
                .expect("valid timestamp literal"),
        ))
    }
}

impl ManualClock {
    /// Moves the clock forward.
    pub fn advance(&self, by: Duration) {
        let mut now = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *now = now
            .checked_add(jiff::SignedDuration::try_from(by).expect("duration fits"))
            .expect("timestamp in range");
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Timestamp {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// A conversation key for `channel` (`platform:instance`) and `room`.
pub fn conversation(channel: &str, room: &str) -> ConversationKey {
    ConversationKey::room(channel.parse().expect("valid channel id"), room)
}

/// A user message in `key`, written by user `u1` ("Ana").
pub fn user_message(key: &ConversationKey, text: &str, at: Timestamp) -> NewMessage {
    NewMessage {
        conversation: key.clone(),
        role: MessageRole::User,
        author_id: Some(UserId::new("u1")),
        author_name: "Ana".into(),
        text: text.into(),
        message_id: None,
        at,
    }
}

/// A bot message in `key`.
pub fn bot_message(key: &ConversationKey, text: &str, at: Timestamp) -> NewMessage {
    NewMessage {
        conversation: key.clone(),
        role: MessageRole::Bot,
        author_id: None,
        author_name: "okbot".into(),
        text: text.into(),
        message_id: None,
        at,
    }
}

mod fakes;
mod memory_store;
pub use fakes::{CollectSink, FakeAgent, FakeChannel, PlainRenderer, fake_cli, inbound, turn};
pub use memory_store::MemoryStore;
