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
    #[serde(default)]
    pub(crate) people: BTreeMap<String, PersonSection>,
    #[serde(default)]
    pub(crate) schedule: ScheduleSection,
    #[serde(default)]
    pub(crate) mcp: Vec<McpSection>,
    #[serde(default)]
    pub(crate) command: Vec<CommandSection>,
}

/// `[[mcp]]`: an extra MCP server (streamable HTTP) the agent may call, next to `kb` and `bot`.
/// The agent sees its tools as `mcp__<name>__<tool>`; every tool of the server is allowed.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct McpSection {
    /// Server name as the agent sees it (`ops`); `kb` and `bot` are taken.
    pub name: String,
    /// Endpoint, `http://` or `https://`.
    pub url: String,
    /// Variable (environment or `.env`) holding the bearer token the server expects.
    pub token_env: String,
    /// Offer the server only in turns of `[bot] admins`.
    #[serde(default)]
    pub admins_only: bool,
    /// What the agent presents: the secret itself, or a token signed with it that names the
    /// requester (and lets the server show prompts).
    #[serde(default)]
    pub auth: McpAuth,
}

/// How the agent authenticates to an `[[mcp]]` server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum McpAuth {
    /// `Authorization: Bearer <secret>`.
    #[default]
    Bearer,
    /// `Authorization: Bearer okbot1.<claims>.<signature>`: who asked, where, until when.
    Signed,
}

/// `[[command]]`: a chat command that calls one tool of an `[[mcp]]` server directly, without the
/// agent: `/status freach` → `company_status {"company": "freach"}`. It answers even when no agent
/// is available, in the same words every time, and works on every platform (text starting with
/// `/`; platforms with native command menus also list it).
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommandSection {
    /// The command without `/`: a-z, 0-9 and `_`, at most 32 (what every platform accepts).
    pub name: String,
    /// One line for `/help` and command menus (at most 100 characters).
    #[serde(default)]
    pub description: String,
    /// The description in other languages, by language code (`{ en = "…", ja = "…" }`): menus show the
    /// one matching the person's app language, `/help` the one matching their reported locale.
    #[serde(default)]
    pub descriptions: BTreeMap<String, String>,
    /// The `[[mcp]]` server.
    pub mcp: String,
    /// The tool to call.
    pub tool: String,
    /// Positional arguments, in order: `company`, `bot?` (optional), `note...` (the rest of the
    /// line). Values are converted to the tool's declared types; `name=value` sets any argument
    /// not fixed by `with`.
    #[serde(default)]
    pub args: Vec<String>,
    /// Arguments always passed (`with = { level = "error" }`); chat cannot change them.
    #[serde(default)]
    pub with: BTreeMap<String, toml::Value>,
    /// Only `[bot] admins` may run it (servers that are `admins_only` imply it).
    #[serde(default)]
    pub admins_only: bool,
}

/// `[schedule]`: reminders and routines (design §5.6).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduleSection {
    /// Send reminders (on by default).
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Where reminders go when their own chat cannot be used (created from the terminal, or the
    /// channel was removed): a channel id such as `discord:main`, used for the owner's direct chat.
    #[serde(default)]
    pub deliver_to: Option<String>,
    /// Firings found this late (the bot was down) are still sent; older ones are marked missed.
    #[serde(default = "default_late_grace")]
    pub late_grace_minutes: u64,
    /// Routine runs (scheduled agent turns) per day, for the whole bot.
    #[serde(default = "default_agent_runs")]
    pub max_agent_runs_per_day: u32,
}

impl Default for ScheduleSection {
    fn default() -> Self {
        Self {
            enabled: true,
            deliver_to: None,
            late_grace_minutes: default_late_grace(),
            max_agent_runs_per_day: default_agent_runs(),
        }
    }
}

fn default_late_grace() -> u64 {
    360
}

fn default_agent_runs() -> u32 {
    24
}

/// `[people.<id>]`: platform accounts of one human. They share one profile and personal memory
/// (`memory/people/<id>/`), so only the operator links accounts.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PersonSection {
    /// Accounts as `platform:instance/user-id` (`/whoami` shows them).
    #[serde(default)]
    pub accounts: Vec<String>,
}

/// Who a bot serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BotProfile {
    /// Many people (a team, a shop's customers): each person has their own memory.
    #[default]
    Team,
    /// One owner: only accounts paired with `okbot pair` are answered, and they share one
    /// profile and memory on every platform.
    Personal,
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
    /// Who the bot serves.
    #[serde(default)]
    pub profile: BotProfile,
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
#[derive(Debug, Clone, PartialEq, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelsSection {
    /// Zalo bots by instance name.
    #[serde(default)]
    pub zalo: BTreeMap<String, ZaloSection>,
    /// Google Chat apps by instance name.
    #[serde(default)]
    pub gchat: BTreeMap<String, GChatSection>,
    /// Discord bots by instance name.
    #[serde(default)]
    pub discord: BTreeMap<String, DiscordSection>,
    /// Slack apps by instance name.
    #[serde(default)]
    pub slack: BTreeMap<String, SlackSection>,
    /// Telegram bots by instance name.
    #[serde(default)]
    pub telegram: BTreeMap<String, TelegramSection>,
    /// Adapters in other languages (bridge protocol), by instance name.
    #[serde(default)]
    pub bridge: BTreeMap<String, BridgeSection>,
}

impl ChannelsSection {
    /// Whether no channel is configured.
    pub fn is_empty(&self) -> bool {
        self.zalo.is_empty()
            && self.gchat.is_empty()
            && self.discord.is_empty()
            && self.slack.is_empty()
            && self.telegram.is_empty()
            && self.bridge.is_empty()
    }

    /// Access settings of every channel, as `(platform, instance, settings)`.
    pub fn access(&self) -> Vec<(&str, &str, ChannelAccess<'_>)> {
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
        for (instance, d) in &self.discord {
            out.push(("discord", instance.as_str(), d.access()));
        }
        for (instance, sl) in &self.slack {
            out.push(("slack", instance.as_str(), sl.access()));
        }
        for (instance, t) in &self.telegram {
            out.push(("telegram", instance.as_str(), t.access()));
        }
        for (instance, b) in &self.bridge {
            out.push((b.platform.as_str(), instance.as_str(), b.access()));
        }
        out
    }
}

macro_rules! access {
    ($t:ty) => {
        impl $t {
            fn access(&self) -> ChannelAccess<'_> {
                ChannelAccess {
                    reply_on: &self.reply_on,
                    allow_users: &self.allow_users,
                    blocked_users: &self.blocked_users,
                    rooms: &self.rooms,
                }
            }
        }
    };
}

/// `[channels.discord.<instance>]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DiscordSection {
    /// Variable holding the bot token.
    pub token_env: String,
    /// Request the privileged message-content intent (enable it in the Developer Portal first).
    #[serde(default)]
    pub message_content: bool,
    /// When to answer.
    #[serde(default = "default_reply_on")]
    pub reply_on: Vec<ReplyOn>,
    /// Only these user ids (all when empty).
    #[serde(default)]
    pub allow_users: Vec<String>,
    /// Never answered.
    #[serde(default)]
    pub blocked_users: Vec<String>,
    /// Per-channel settings (`room` is the Discord channel id).
    #[serde(default)]
    pub rooms: Vec<RoomSection>,
}

/// `[channels.slack.<instance>]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SlackSection {
    /// Variable holding the app-level token (`xapp-…`, scope `connections:write`).
    pub app_token_env: String,
    /// Variable holding the bot token (`xoxb-…`).
    pub bot_token_env: String,
    /// When to answer.
    #[serde(default = "default_reply_on")]
    pub reply_on: Vec<ReplyOn>,
    /// Only these user ids (all when empty).
    #[serde(default)]
    pub allow_users: Vec<String>,
    /// Never answered.
    #[serde(default)]
    pub blocked_users: Vec<String>,
    /// Per-channel settings (`room` is the Slack channel id).
    #[serde(default)]
    pub rooms: Vec<RoomSection>,
}

/// `[channels.bridge.<instance>]`: an adapter process speaking the bridge protocol.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BridgeSection {
    /// Platform name: the channel id is `<platform>:<instance>`.
    pub platform: String,
    /// Executable; default `okbot-channel-<platform>` on `PATH`.
    #[serde(default)]
    pub command: Option<String>,
    /// Arguments.
    #[serde(default)]
    pub args: Vec<String>,
    /// Variables (from `.env` or the environment) passed to the adapter, by name.
    #[serde(default)]
    pub env: Vec<String>,
    /// Adapter settings, sent in `initialize`.
    #[serde(default)]
    pub settings: toml::Table,
    /// Longest message the platform accepts.
    #[serde(default = "default_bridge_len")]
    pub max_message_len: usize,
    /// Formatting dialect (`common-mark`, `plain`, `slack-mrkdwn`, …).
    #[serde(default = "default_bridge_dialect")]
    pub dialect: okbot_core::MarkdownDialect,
    /// Replies can go into threads.
    #[serde(default)]
    pub threads: bool,
    /// The adapter supports `edit`.
    #[serde(default)]
    pub edit: bool,
    /// The adapter supports `typing`.
    #[serde(default)]
    pub typing: bool,
    /// When to answer.
    #[serde(default = "default_reply_on")]
    pub reply_on: Vec<ReplyOn>,
    /// Only these user ids (all when empty).
    #[serde(default)]
    pub allow_users: Vec<String>,
    /// Never answered.
    #[serde(default)]
    pub blocked_users: Vec<String>,
    /// Per-room settings.
    #[serde(default)]
    pub rooms: Vec<RoomSection>,
}

fn default_bridge_len() -> usize {
    2000
}

fn default_bridge_dialect() -> okbot_core::MarkdownDialect {
    okbot_core::MarkdownDialect::CommonMark
}

access!(BridgeSection);
access!(DiscordSection);
access!(SlackSection);
access!(TelegramSection);

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

/// `[channels.telegram.<instance>]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TelegramSection {
    /// Variable holding the bot token from @BotFather.
    pub token_env: String,
    /// Receiving mode.
    #[serde(default)]
    pub delivery: Delivery,
    /// Public HTTPS URL for webhook delivery.
    #[serde(default)]
    pub public_url: Option<String>,
    /// Variable holding the webhook secret (`A-Z a-z 0-9 _ -`, 1–256 characters).
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
    /// Per-chat settings (`room` is the chat id, negative for groups).
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
    /// A group where answers may use the asker's personal memory, tasks and notes (for example
    /// a family group). Off by default: everyone in a group reads the answers.
    #[serde(default)]
    pub personal: bool,
}

fn default_true() -> bool {
    true
}
