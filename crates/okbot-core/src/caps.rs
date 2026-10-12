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
