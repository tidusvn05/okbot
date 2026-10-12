//! The `okbot.toml` schema. Unknown keys are errors.

use std::collections::BTreeMap;
use std::path::PathBuf;

use okbot_core::ToolPolicy;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct File {
    pub(crate) config_version: u32,
    pub(crate) bot: BotSection,
    #[serde(default)]
    pub(crate) agent: AgentSection,
    #[serde(default)]
    pub(crate) http: HttpSection,
    #[serde(default)]
    pub(crate) channels: ChannelsSection,
}

/// `[bot]`: identity and locations.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BotSection {
    /// The bot's name, used in prompts and replies.
    pub name: String,
    /// Knowledge bundle directory.
    #[serde(default = "default_kb")]
    pub kb: PathBuf,
    /// State directory (database, agent working directories, index).
    #[serde(default = "default_state")]
    pub state_dir: PathBuf,
    /// Administrators as `platform:instance/user-id`.
    #[serde(default)]
    pub admins: Vec<String>,
    /// IANA timezone shown to the agent.
    #[serde(default)]
    pub timezone: Option<String>,
    /// Chat history older than this is deleted.
    #[serde(default = "default_retention")]
    pub retention_days: u32,
}

fn default_kb() -> PathBuf {
    "kb".into()
}

fn default_state() -> PathBuf {
    "state".into()
}

fn default_retention() -> u32 {
    90
}

/// `[agent]`: which agent CLIs answer and how.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSection {
    /// Agent CLIs in fallback order.
    #[serde(default = "default_chain")]
    pub chain: Vec<String>,
    /// Upper bound per answer, in seconds.
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    /// Agent processes running at once for this bot.
    #[serde(default = "default_concurrency")]
    pub max_concurrent: usize,
    /// Default tool policy for every room.
    #[serde(default)]
    pub tool_policy: ToolPolicy,
    /// Approximate token budget for chat history in the prompt.
    #[serde(default = "default_history_tokens")]
    pub history_tokens: usize,
    /// Allow agents that cannot restrict their tools (Devin, OpenCode). Never for public bots.
    #[serde(default)]
    pub allow_unrestricted_agents: bool,
    /// A separate home for the agent CLIs (their own login and quota). Log in once with
    /// `HOME=<home> claude` (or `codex login`). Unset: the user's own login, shared by all bots.
    #[serde(default)]
    pub home: Option<PathBuf>,
    /// Per-CLI settings.
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderSection>,
}

impl Default for AgentSection {
    fn default() -> Self {
        Self {
            chain: default_chain(),
            timeout_secs: default_timeout(),
            max_concurrent: default_concurrency(),
            tool_policy: ToolPolicy::default(),
            history_tokens: default_history_tokens(),
            allow_unrestricted_agents: false,
            home: None,
            providers: BTreeMap::new(),
        }
    }
}

fn default_chain() -> Vec<String> {
    vec!["claude".into()]
}

fn default_timeout() -> u64 {
    180
}

fn default_concurrency() -> usize {
    2
}

fn default_history_tokens() -> usize {
    6000
}

/// `[agent.providers.<name>]`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderSection {
    /// Absolute path of the CLI (recorded by `okbot init`); `None` searches `PATH`.
    #[serde(default)]
    pub cli: Option<PathBuf>,
    /// Model in the CLI's format (`sonnet@high`).
    #[serde(default)]
    pub model: Option<String>,
}

/// `[http]`: the server for webhooks and health checks.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HttpSection {
    /// Listen address (`127.0.0.1:8787`). Needed for webhook channels.
    #[serde(default)]
    pub listen: Option<String>,
}

/// `[channels.<platform>.<instance>]`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelsSection {
    /// Zalo bots by instance name.
    #[serde(default)]
    pub zalo: BTreeMap<String, ZaloSection>,
    /// Google Chat apps by instance name.
    #[serde(default)]
    pub gchat: BTreeMap<String, GChatSection>,
}

impl ChannelsSection {
    /// Whether no channel is configured.
    pub fn is_empty(&self) -> bool {
        self.zalo.is_empty() && self.gchat.is_empty()
    }

    /// Access settings of every channel, as `(platform, instance, settings)`.
    pub fn access(&self) -> Vec<(&'static str, &str, ChannelAccess<'_>)> {
        let mut out = Vec::new();
        for (instance, z) in &self.zalo {
            out.push((
                "zalo",
                instance.as_str(),
                ChannelAccess {
                    reply_on: &z.reply_on,
                    allow_users: &z.allow_users,
                    blocked_users: &z.blocked_users,
                    rooms: &z.rooms,
                },
            ));
        }
        for (instance, g) in &self.gchat {
            out.push((
                "gchat",
                instance.as_str(),
                ChannelAccess {
                    reply_on: &g.reply_on,
                    allow_users: &g.allow_users,
                    blocked_users: &g.blocked_users,
                    rooms: &g.rooms,
                },
            ));
        }
        out
    }
}

/// The access settings every channel section has.
#[derive(Debug, Clone, Copy)]
pub struct ChannelAccess<'a> {
    /// When to answer.
    pub reply_on: &'a [ReplyOn],
    /// Only these users (all when empty).
    pub allow_users: &'a [String],
    /// Never these users.
    pub blocked_users: &'a [String],
    /// Per-room settings.
    pub rooms: &'a [RoomSection],
}

/// How Google Chat delivers events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum GChatDelivery {
    /// Cloud Pub/Sub pull subscription (no public URL).
    #[default]
    Pubsub,
    /// HTTPS endpoint through `[http]`.
    Http,
}

/// `[channels.gchat.<instance>]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GChatSection {
    /// Service-account key file (relative to the bot directory), kept out of git.
    pub credentials: PathBuf,
    /// Receiving mode.
    #[serde(default)]
    pub delivery: GChatDelivery,
    /// `projects/<p>/subscriptions/<s>` for Pub/Sub delivery.
    #[serde(default)]
    pub subscription: Option<String>,
    /// The app's endpoint URL, when its authentication audience is "HTTP endpoint URL".
    #[serde(default)]
    pub audience_url: Option<String>,
    /// The project number, when the authentication audience is "Project number".
    #[serde(default)]
    pub audience_project_number: Option<String>,
    /// When to answer.
    #[serde(default = "default_reply_on")]
    pub reply_on: Vec<ReplyOn>,
    /// Only these users (`users/<id>`) are answered (all when empty).
    #[serde(default)]
    pub allow_users: Vec<String>,
    /// Never answered.
    #[serde(default)]
    pub blocked_users: Vec<String>,
    /// Per-space settings (`room` is the space name, `spaces/…`).
    #[serde(default)]
    pub rooms: Vec<RoomSection>,
}

/// How a channel receives events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Delivery {
    /// Long polling.
    #[default]
    Polling,
    /// Webhook through `[http]`.
    Webhook,
}

/// When the bot answers in a channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReplyOn {
    /// One-to-one chats.
    Dm,
    /// Group messages that mention the bot.
    Mention,
    /// Every group message.
    All,
}

/// `[channels.zalo.<instance>]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ZaloSection {
    /// Variable holding the bot token.
    pub token_env: String,
    /// Receiving mode.
    #[serde(default)]
    pub delivery: Delivery,
    /// Public HTTPS URL for webhook delivery.
    #[serde(default)]
    pub public_url: Option<String>,
    /// Variable holding the webhook secret.
    #[serde(default)]
    pub secret_env: Option<String>,
    /// When to answer.
    #[serde(default = "default_reply_on")]
    pub reply_on: Vec<ReplyOn>,
    /// Only these user ids are answered (all when empty).
    #[serde(default)]
    pub allow_users: Vec<String>,
    /// Never answered.
    #[serde(default)]
    pub blocked_users: Vec<String>,
    /// Per-room settings.
    #[serde(default)]
    pub rooms: Vec<RoomSection>,
}

fn default_reply_on() -> Vec<ReplyOn> {
    vec![ReplyOn::Dm, ReplyOn::Mention]
}

/// `[[channels.<platform>.<instance>.rooms]]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RoomSection {
    /// Room (chat) id.
    pub room: String,
    /// Tool policy for this room.
    #[serde(default)]
    pub tool_policy: Option<ToolPolicy>,
    /// The room may see `audience: private` knowledge.
    #[serde(default)]
    pub internal: bool,
}
