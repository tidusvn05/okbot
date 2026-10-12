//! Messages flowing through the pipeline.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::id::{ConversationKey, MessageId, UserRef};

/// Media attached to a message. Platforms that deliver media do so by URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[non_exhaustive]
pub enum Attachment {
    /// An image.
    Image {
        /// Where to fetch it.
        url: String,
        /// Text sent with the image.
        caption: Option<String>,
    },
    /// A voice or audio message.
    Audio {
        /// Where to fetch it.
        url: String,
    },
    /// A sticker.
    Sticker {
        /// Platform sticker id or URL.
        id: String,
    },
    /// Any other file.
    File {
        /// Where to fetch it.
        url: String,
        /// File name, when known.
        name: Option<String>,
    },
}

/// A message received from a platform, normalized by its channel adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboundEvent {
    /// Platform event or message id, used to drop duplicate deliveries.
    pub event_id: String,
    /// The conversation it belongs to.
    pub conversation: ConversationKey,
    /// Who wrote it.
    pub author: UserRef,
    /// The text, with mentions of the bot removed and other mentions written as `@name`.
    pub text: String,
    /// Media sent with the message.
    pub attachments: Vec<Attachment>,
    /// The bot was mentioned (or replied to, on platforms that only deliver such group messages).
    pub mentions_bot: bool,
    /// A one-to-one conversation with the bot.
    pub is_direct: bool,
    /// The platform withheld the content (for example Zalo's `message.unsupported.received` for
    /// protected users). The runtime answers with a fixed message and never calls an agent.
    pub restricted: bool,
    /// Platform message id, used to reply to this message where supported.
    pub message_id: Option<MessageId>,
    /// When the platform says it was sent (or when it was received, if unknown).
    pub sent_at: Timestamp,
}

/// Where a reply goes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyTarget {
    /// The conversation to post into.
    pub conversation: ConversationKey,
    /// The message being answered, on platforms that can quote or thread under it.
    pub in_reply_to: Option<MessageId>,
}

impl ReplyTarget {
    /// The target that answers `event`.
    pub fn to_event(event: &InboundEvent) -> Self {
        Self {
            conversation: event.conversation.clone(),
            in_reply_to: event.message_id.clone(),
        }
    }
}

/// A reply in canonical Markdown, before rendering for a platform.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct OutboundMessage {
    /// CommonMark text.
    pub markdown: String,
    /// Media to send.
    pub attachments: Vec<Attachment>,
}

impl OutboundMessage {
    /// A text-only message.
    pub fn text(markdown: impl Into<String>) -> Self {
        Self {
            markdown: markdown.into(),
            attachments: Vec::new(),
        }
    }
}

/// One platform message, already in the platform's dialect and within its length limit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderedMessage {
    /// Text in the channel's [`MarkdownDialect`](crate::MarkdownDialect).
    pub text: String,
}

/// A message the bot sent, for later edits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SentRef {
    /// Where it was sent.
    pub target: ReplyTarget,
    /// The platform's id for it.
    pub message_id: MessageId,
}
