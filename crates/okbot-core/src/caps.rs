//! Capabilities of channels and agents, and the tool policy.

use serde::{Deserialize, Serialize};

/// The Markdown flavour a platform displays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum MarkdownDialect {
    /// CommonMark, shown as-is (console).
    CommonMark,
    /// Zalo Bot `parse_mode = "markdown"`: no links, code blocks or tables.
    ZaloMd,
    /// Google Chat text formatting.
    GChatText,
    /// Discord Markdown.
    DiscordMd,
    /// Slack mrkdwn.
    SlackMrkdwn,
    /// Telegram `parse_mode = "HTML"`: `<b>`, `<i>`, `<s>`, `<code>`, `<pre>`, `<a>`,
    /// `<blockquote>`; no tables or headings.
    TelegramHtml,
    /// No formatting at all.
    Plain,
}

/// How a platform counts message length.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum LengthUnit {
    /// Unicode scalar values (`char`s).
    Chars,
    /// UTF-16 code units (JavaScript/Java string length).
    Utf16,
}

/// What a channel can do. The runtime adapts its behaviour to these values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelCaps {
    /// Longest message the platform accepts, in [`ChannelCaps::length_unit`].
    pub max_message_len: usize,
    /// How the platform counts length.
    pub length_unit: LengthUnit,
    /// Formatting dialect.
    pub dialect: MarkdownDialect,
    /// Whether replies can go into threads.
    pub threads: bool,
    /// Whether sent messages can be edited (enables "thinking…" and progressive replies).
    pub edit: bool,
    /// Whether a typing indicator exists.
    pub typing: bool,
    /// What interactive replies can use (ADR 0015).
    #[serde(default)]
    pub ui: UiCaps,
}

/// The parts of a [`Prompt`](crate::Prompt) a channel shows natively. Whatever is missing is
/// shown as text (cards, a numbered menu, a list of fields to type).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UiCaps {
    /// Buttons, including link buttons.
    pub buttons: bool,
    /// A menu of options ([`Select`](crate::Select)).
    pub select: bool,
    /// Forms behind buttons.
    pub forms: bool,
    /// Cards with a title, shown apart from the text.
    pub cards: bool,
    /// A card's chart as a picture ([`Card::image`](crate::Card::image), ADR 0016).
    pub images: bool,
}

impl UiCaps {
    /// Everything (Discord, Slack).
    pub fn all() -> Self {
        Self {
            buttons: true,
            select: true,
            forms: true,
            cards: true,
            images: true,
        }
    }
}

impl ChannelCaps {
    /// Conservative defaults: plain CommonMark, 2000 chars, nothing optional.
    pub fn basic() -> Self {
        Self {
            max_message_len: 2000,
            length_unit: LengthUnit::Chars,
            dialect: MarkdownDialect::CommonMark,
            threads: false,
            edit: false,
            typing: false,
            ui: UiCaps::default(),
        }
    }
}

/// Which tools an agent may use in a room (design §4.4). No policy grants a shell or file access.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum ToolPolicy {
    /// okbot's MCP tools plus the agent CLI's built-in web search and fetch. The default.
    #[default]
    Web,
    /// okbot's MCP tools only.
    McpOnly,
}

/// How the bot answers (ADR 0015).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum ReplyMode {
    /// Chat text only. The default.
    #[default]
    Text,
    /// Text, plus cards, buttons, menus and forms the agent shows with its `show` tool, natively
    /// where the platform has them and as text elsewhere.
    Interactive,
}

/// What an agent backend can do natively. Missing capabilities are emulated or the backend is skipped.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AgentCaps {
    /// Accepts a separate system prompt (otherwise it is prepended to the prompt).
    pub system_prompt: bool,
    /// Accepts MCP servers per run.
    pub mcp: bool,
    /// Tool policies the backend can enforce. A run is never started with an unenforceable policy.
    pub tool_policies: Vec<ToolPolicy>,
    /// Can resume a native session.
    pub resume: bool,
    /// Can stream partial output.
    pub stream: bool,
}

impl AgentCaps {
    /// Whether `policy` can be enforced.
    pub fn enforces(&self, policy: ToolPolicy) -> bool {
        self.tool_policies.contains(&policy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_policy_names() {
        assert_eq!(
            serde_json::to_string(&ToolPolicy::McpOnly).unwrap(),
            "\"mcp-only\""
        );
        assert_eq!(ToolPolicy::default(), ToolPolicy::Web);
    }
}
