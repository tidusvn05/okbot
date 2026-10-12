//! The renderer port.

use crate::caps::ChannelCaps;
use crate::message::RenderedMessage;

/// Turns canonical Markdown into platform messages: the platform's dialect, each part within its
/// length limit.
pub trait Renderer: Send + Sync {
    /// Renders `markdown` for a channel with `caps`. Returns at least one message for non-empty input.
    fn render(&self, markdown: &str, caps: &ChannelCaps) -> Vec<RenderedMessage>;
}
