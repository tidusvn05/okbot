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
    #[serde(default)]
    pub(crate) notify: Vec<NotifySection>,
    #[serde(default)]
    pub(crate) roles: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub(crate) limits: LimitsSection,
    #[serde(default)]
    pub(crate) automation: Vec<AutomationSection>,
    #[serde(default)]
    pub(crate) teams: BTreeMap<String, TeamSection>,
}

/// `[teams.<id>]`: this bot in a team of okbots that delegate work to each other (ADR 0014). The
/// team's manifest says who the members are; this section says what they may do here.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TeamSection {
    /// Where the manifest is: an `https://` URL (signed with `team_key`), or a file (default:
    /// `~/.config/okbot/teams/<id>.toml`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub manifest: String,
    /// The team's public key (`ed25519:…`): a fetched manifest must be signed with it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub team_key: String,
    /// This bot's member name; default: the `[bot] name` as a slug.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub member: String,
    /// Other members may delegate work to this bot.
    #[serde(default)]
    pub accept: bool,
    /// The caller's team roles mapped to roles of this bot (`[roles]` or `everyone`).
    #[serde(default)]
    pub roles: BTreeMap<String, Vec<String>>,
    /// Who in this bot's chats may make the agent delegate (`[roles]`, `admins`, `everyone`).
    #[serde(default = "admins_only")]
    pub delegate_roles: Vec<String>,
    /// `always`: the requester approves each delegation first.
    #[serde(default)]
    pub confirm: Confirm,
    /// The longest chain of delegations accepted (a → b → c is 2).
    #[serde(default = "default_depth")]
    pub max_depth: u32,
    /// Inbound tasks running at once, per calling member.
    #[serde(default = "default_concurrent")]
    pub max_concurrent: u32,
    /// Seconds `delegate` waits before the task continues in the background.
    #[serde(default = "default_wait")]
    pub wait: u64,
    /// Seconds a delegated task may take (its deadline), unless the chain's is sooner.
    #[serde(default = "default_task_timeout")]
    pub task_timeout: u64,
}

fn admins_only() -> Vec<String> {
    vec!["admins".into()]
}

fn default_depth() -> u32 {
    2
}

fn default_concurrent() -> u32 {
    2
}

fn default_wait() -> u64 {
    60
}

fn default_task_timeout() -> u64 {
    600
}

/// `[limits]`: what one person may use over a rolling 24 hours (ADR 0011). Administrators are
/// exempt; zero means no limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LimitsSection {
    /// Agent turns per person.
    #[serde(default)]
    pub turns_per_day: u32,
    /// Input plus output tokens per person, as the agents report them.
    #[serde(default)]
    pub tokens_per_day: u64,
}

/// `[[mcp]]`: an extra MCP server (streamable HTTP) the agent may call, next to `kb` and `bot`.
/// The agent sees its tools as `mcp__<name>__<tool>` and reaches them through okbot's gateway,
/// which applies the rules below (ADR 0011).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct McpSection {
    /// Server name as the agent sees it (`ops`); `kb` and `bot` are taken.
    pub name: String,
    /// Endpoint, `http://` or `https://` (or `command` for a local server).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub url: String,
    /// Variable (environment or `.env`) holding the bearer token the server expects (with `url`;
    /// not with `auth = "oauth"`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub token_env: String,
    /// A local server speaking MCP over stdio, started for each agent session:
    /// `["npx", "-y", "@scope/server"]`. It gets an empty environment and directory.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
    /// Variables (environment or `.env`) passed to the `command` server, by name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<String>,
    /// Offer the server only in turns of `[bot] admins`.
    #[serde(default)]
    pub admins_only: bool,
    /// What the agent presents: the secret itself, or a token signed with it that names the
    /// requester (and lets the server show prompts).
    #[serde(default)]
    pub auth: McpAuth,
    /// Roles that may use the server (`[roles]`, `admins`, `everyone`); empty: everyone.
    #[serde(default)]
    pub roles: Vec<String>,
    /// Rooms where the server is offered, as `platform:instance/room-id`; empty: everywhere.
    #[serde(default)]
    pub rooms: Vec<String>,
    /// Tools offered, as names or patterns with `*`; empty: every tool.
    #[serde(default)]
    pub tools: Vec<String>,
    /// Tools never offered (wins over `tools`).
    #[serde(default)]
    pub deny_tools: Vec<String>,
    /// Tools only some roles may call.
    #[serde(default)]
    pub restrict: Vec<RestrictSection>,
    /// Which calls the agent makes need a person's approval first.
    #[serde(default)]
    pub confirm: Confirm,
    /// Who approves them.
    #[serde(default)]
    pub confirm_by: ConfirmBy,
    /// With `auth = "oauth"`: whose account the server reads (ADR 0012).
    #[serde(default, skip_serializing_if = "is_default")]
    pub credentials: McpCredentials,
    /// With `auth = "oauth"`: a client registered by hand, for servers without dynamic client
    /// registration, and the scopes to ask for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth: Option<OAuthSection>,
}

fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    *value == T::default()
}

/// Whose credentials an `auth = "oauth"` server uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum McpCredentials {
    /// Each person connects their own account (`/connect`); used only in their direct chats and
    /// `personal` rooms.
    #[default]
    Person,
    /// An administrator connects one account for everyone the server's rules allow.
    Shared,
}

/// `[mcp.oauth]`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OAuthSection {
    /// A client id registered with the provider (otherwise okbot registers itself).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    /// Variable holding that client's secret, for confidential clients.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret_env: Option<String>,
    /// Scopes to ask for; empty: what the server's metadata lists.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
}

/// `[[automation]]`: when something happens, read a source and act (ADR 0013).
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationSection {
    /// The name in logs, `okbot automation` and `/automation/<name>`: a-z, 0-9, `-` and `_`.
    pub name: String,
    /// Whose credentials, roles and direct chat it uses: `person:<id>` or an account
    /// `platform:instance/user-id`. Defaults to the owner on a personal bot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_as: Option<String>,
    /// Poll the source this often (`5m`, `1h`, `1d`) and act on new items.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every: Option<String>,
    /// Act at these times (`weekdays 08:00`, `daily 18:30`, `every 3h`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,
    /// Act when a program posts to `/automation/<name>` with `token_env`'s bearer token.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub webhook: bool,
    /// Variable holding the webhook's bearer token.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub token_env: String,
    /// The tool whose result is the input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceSection>,
    /// Send this text (Markdown; `{{item}}`, `{{item.field}}`, `{{count}}`, `{{name}}`) to `to`,
    /// without an agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Ask the agent this, as `run_as`, with the items appended as data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Where it goes, as `[[notify]] to` targets (a prompt: at most one; default `run_as`'s
    /// direct chat).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub to: Vec<String>,
    /// Runs that act, per day (default 24 with a prompt, 200 with a text).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_runs_per_day: Option<u32>,
    /// New items acted on per run (1 to 100); more wait for the next run.
    #[serde(default = "default_max_items")]
    pub max_items: u32,
    /// Off until set to false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub paused: bool,
}

fn default_max_items() -> u32 {
    20
}

/// `[automation.source]`: one `[[mcp]]` tool, called without the agent.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceSection {
    /// The `[[mcp]]` server (an HTTP one).
    pub mcp: String,
    /// The tool.
    pub tool: String,
    /// Its arguments.
    #[serde(default)]
    pub with: BTreeMap<String, toml::Value>,
    /// JSON pointer to the array of items in the result (`/messages`); unset: the whole text is
    /// one item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items: Option<String>,
    /// JSON pointer to an item's id (`/id`); unset: the item's content identifies it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

/// `[[mcp.restrict]]`: tools matching `tools` may only be called by `roles`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RestrictSection {
    /// Names or patterns with `*`.
    pub tools: Vec<String>,
    /// Roles allowed to call them.
    pub roles: Vec<String>,
}

/// Which agent calls of an `[[mcp]]` server wait for a person's approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Confirm {
    /// None.
    #[default]
    Never,
    /// Tools that are not read-only and do not say they are harmless (MCP tool annotations).
    Destructive,
    /// Every call.
    Always,
}

/// Who approves a call that needs confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConfirmBy {
    /// The person whose message started the turn.
    #[default]
    Requester,
    /// A `[bot] admin` in the same chat.
    Admins,
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
    /// OAuth 2.1 as the MCP specification describes it: people connect with `/connect`.
    Oauth,
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
    /// Roles that may run it; empty: whoever may use the tool on its server.
    #[serde(default)]
    pub roles: Vec<String>,
}

/// `[[notify]]`: a named destination other programs post to (ADR 0011). `POST /notify/<name>` with
/// `Authorization: Bearer <token>` sends a message to every target in `to`, rendered for each
/// platform; callers cannot choose other chats.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NotifySection {
    /// The name in the URL (`alerts`): a-z, 0-9, `-` and `_`.
    pub name: String,
    /// Variable (environment or `.env`) holding the bearer token callers send. Never one that an
    /// agent receives (a bearer `[[mcp]]` token).
    pub token_env: String,
    /// Where messages go: `room:<conversation>` (as `/whoami` shows it),
    /// `user:<platform>:<instance>/<user id>`, `person:<id>` or `admins`.
    pub to: Vec<String>,
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
    /// The public HTTPS address of `listen` behind a reverse proxy (`https://bot.example.com`),
    /// for OAuth callbacks (`/oauth/callback`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_url: Option<String>,
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
