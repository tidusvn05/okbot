//! Renders the agent's Markdown into each chat platform's dialect and splits long replies.
//!
//! Agents answer in CommonMark. Each platform shows a different subset: Zalo has no links, code
//! blocks or tables; Google Chat and Slack use `*bold*` and `<url|text>`; and every platform caps
//! message length. [`MarkdownRenderer`] parses the answer once, writes it in the channel's
//! [`MarkdownDialect`](okbot_core::MarkdownDialect) and packs the result into messages that fit,
//! never splitting a code block without re-opening its fence.
//!
//! ```
//! use okbot_core::{ChannelCaps, LengthUnit, MarkdownDialect, Renderer};
//! use okbot_render::MarkdownRenderer;
//!
//! let caps = ChannelCaps {
//!     max_message_len: 2000,
//!     length_unit: LengthUnit::Utf16,
//!     dialect: MarkdownDialect::ZaloMd,
//!     ..ChannelCaps::basic()
//! };
//! let parts = MarkdownRenderer.render("See [docs](https://example.com) for **details**.", &caps);
//! assert_eq!(parts[0].text, "See docs (https://example.com) for **details**.");
//! ```

mod emit;
mod ir;
mod split;

use okbot_core::{ChannelCaps, RenderedMessage, Renderer};

/// The default [`Renderer`]: CommonMark in, platform dialect out.
#[derive(Debug, Clone, Copy, Default)]
pub struct MarkdownRenderer;

impl Renderer for MarkdownRenderer {
    fn render(&self, markdown: &str, caps: &ChannelCaps) -> Vec<RenderedMessage> {
        let blocks = ir::parse(markdown);
        let chunks = emit::emit(&blocks, caps.dialect);
        split::pack(chunks, caps.max_message_len, caps.length_unit)
            .into_iter()
            .map(|text| RenderedMessage { text })
            .collect()
    }
}
