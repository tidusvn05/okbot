//! Domain model and ports (traits) of okbot; the SDK for third-party adapters.
//!
//! okbot follows a ports-and-adapters architecture (ADR 0001). This crate holds the types every
//! other crate speaks and the traits adapters implement. It performs no I/O.
//!
//! | Port | Implemented by | Used by |
//! |---|---|---|
//! | [`Channel`] | one crate per chat platform | the runtime |
//! | [`InboundSink`] | the runtime | channels, to deliver events |
//! | [`AgentBackend`] | agent CLIs, fallback chains | the runtime |
//! | [`ConversationStore`] | SQLite store | the runtime |
//! | [`KnowledgeProvider`] | okbase integration | the runtime |
//! | [`Renderer`] | Markdown dialect renderer | the runtime |
//! | [`Clock`] | [`SystemClock`], test clocks | everything that reads time |
//!
//! # Writing a channel
//!
//! A platform adapter implements [`Channel`]: receive events and hand them to the [`InboundSink`],
//! send rendered replies. Everything else (policy, agents, knowledge) is the runtime's job.
//!
//! ```
//! use std::sync::Arc;
//! use okbot_core::{
//!     async_trait, CancellationToken, Channel, ChannelCaps, ChannelError, ChannelId, InboundSink,
//!     MessageId, RenderedMessage, ReplyTarget, SentRef,
//! };
//!
//! struct EchoChannel { id: ChannelId, caps: ChannelCaps }
//!
//! #[async_trait]
//! impl Channel for EchoChannel {
//!     fn id(&self) -> &ChannelId { &self.id }
//!     fn caps(&self) -> &ChannelCaps { &self.caps }
//!
//!     async fn run(&self, _sink: Arc<dyn InboundSink>, shutdown: CancellationToken)
//!         -> Result<(), ChannelError> {
//!         // Connect to the platform and call `sink.deliver(event)` for each message.
//!         shutdown.cancelled().await;
//!         Ok(())
//!     }
//!
//!     async fn send(&self, to: &ReplyTarget, message: &RenderedMessage)
//!         -> Result<SentRef, ChannelError> {
//!         println!("{}", message.text);
//!         Ok(SentRef { target: to.clone(), message_id: MessageId::new("1") })
//!     }
//! }
//!
//! let channel = EchoChannel { id: "echo:demo".parse().unwrap(), caps: ChannelCaps::basic() };
//! assert_eq!(channel.id().to_string(), "echo:demo");
//! ```
#![deny(missing_docs)]

mod agent;
mod caps;
mod channel;
mod clock;
mod id;
mod knowledge;
mod message;
mod render;
mod secret;
mod store;
mod webhook;

pub use agent::{AgentBackend, AgentError, AgentReply, AgentTurn, McpServer, TokenUsage};
pub use caps::{AgentCaps, ChannelCaps, LengthUnit, MarkdownDialect, ToolPolicy};
pub use channel::{Channel, ChannelError, InboundSink, SinkClosed};
pub use clock::{Clock, SystemClock};
pub use id::{ChannelId, ConversationKey, IdError, MessageId, RunId, UserId, UserRef};
pub use knowledge::{KnowledgeContext, KnowledgeError, KnowledgeProvider, Requester, RunGrant};
pub use message::{
    Attachment, InboundEvent, OutboundMessage, RenderedMessage, ReplyTarget, SentRef,
};
pub use render::Renderer;
pub use secret::{Redactor, Secret};
pub use store::{
    AgentRunRecord, ConversationStore, MessageRole, NewMessage, StoreError, StoredMessage,
};
pub use webhook::{WebhookHandler, WebhookRequest, WebhookResponse};

/// Re-exported so adapters use the same cancellation type as the runtime.
pub use tokio_util::sync::CancellationToken;

/// Re-exported so adapters implement the async traits with the same macro version.
pub use async_trait::async_trait;

/// A boxed error from an adapter, kept as the `source` of port errors.
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;
