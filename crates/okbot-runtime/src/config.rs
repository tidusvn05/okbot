//! Runtime settings. `okbot` builds these from `okbot.toml`.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use okbot_core::{ChannelId, McpServer, Person, Secret, ToolPolicy, UserId};

/// When the bot answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyOn {
    /// In one-to-one conversations.
    pub direct: bool,
    /// When mentioned (or replied to) in group conversations.
    pub mention: bool,
    /// Every group message, mentioned or not.
    pub all: bool,
}

impl Default for ReplyOn {
    fn default() -> Self {
        Self {
            direct: true,
            mention: true,
            all: false,
        }
    }
}

/// Per-user rate limit: at most `messages` per `window`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimit {
    /// Messages allowed per window.
    pub messages: u32,
    /// Window length.
    pub window: Duration,
}

impl Default for RateLimit {
    fn default() -> Self {
        Self {
            messages: 20,
            window: Duration::from_secs(60),
        }
    }
}

/// Who may talk to the bot.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccessPolicy {
    /// When to answer (channels without an entry in `reply_on_by_channel`).
    pub reply_on: ReplyOn,
    /// When to answer, per channel.
    pub reply_on_by_channel: HashMap<ChannelId, ReplyOn>,
    /// If set, only these users (per channel) are answered.
    pub allow_users: Option<HashSet<(ChannelId, UserId)>>,
    /// If set, only these rooms (per channel) are answered.
    pub allow_rooms: Option<HashSet<(ChannelId, String)>>,
    /// Never answered.
    pub blocked_users: HashSet<(ChannelId, UserId)>,
    /// Administrators (may use admin commands).
    pub admins: HashSet<(ChannelId, UserId)>,
    /// Messages from other bots are answered too (off by default to avoid bot loops).
    pub answer_bots: bool,
    /// Per-user rate limit.
    pub rate_limit: RateLimit,
    /// Longer messages are refused.
    pub max_input_chars: usize,
}

/// Settings for one room.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RoomRule {
    /// Tool policy override.
    pub tool_policy: Option<ToolPolicy>,
    /// The room may see `audience: private` knowledge.
    pub internal: bool,
    /// A group where answers may use the asker's personal profile and data.
    pub personal: bool,
}

/// Fixed texts the bot sends without asking an agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixedReplies {
    /// For platform-withheld (restricted) messages.
    pub restricted: String,
    /// When every agent failed.
    pub error: String,
    /// When a user is over the rate limit.
    pub rate_limited: String,
    /// When a message is too long.
    pub too_long: String,
    /// After `/stop` cancelled a turn.
    pub stopped: String,
    /// Shown while an answer is written, on platforms that edit messages but have no typing
    /// indicator (Google Chat).
    pub thinking: String,
    /// To accounts that are not the owner's, on a personal bot.
    pub private: String,
}

impl Default for FixedReplies {
    fn default() -> Self {
        Self {
            restricted: "Sorry, I can't read this message.".into(),
            error: "Sorry, something went wrong while answering. Please try again in a moment."
                .into(),
            rate_limited: "You're sending messages quickly. Please wait a minute and try again."
                .into(),
            too_long: "That message is too long for me. Please shorten it.".into(),
            stopped: "Stopped.".into(),
            thinking: "_Thinking…_".into(),
            private: "This is a private assistant. If it is yours, run `okbot pair` where the bot \
                      runs, then send me `/pair <code>` in a direct message."
                .into(),
        }
    }
}

/// Everything the runtime needs besides its ports.
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    /// The bot's name, used in prompts and as the author of its messages.
    pub bot_name: String,
    /// Parent of the per-conversation agent working directories.
    pub work_dir: PathBuf,
    /// Upper bound for one agent turn.
    pub agent_timeout: Duration,
    /// Messages loaded from history per turn (before token trimming).
    pub history_messages: usize,
    /// Approximate token budget for history in the prompt.
    pub history_tokens: usize,
    /// How long inbound event ids are remembered for dedupe.
    pub dedupe_ttl: Duration,
    /// How often the typing indicator is repeated.
    pub typing_interval: Duration,
    /// Idle conversation workers stop after this long.
    pub worker_idle: Duration,
    /// How long shutdown waits for running turns.
    pub shutdown_grace: Duration,
    /// Access rules.
    pub access: AccessPolicy,
    /// Default tool policy.
    pub tool_policy: ToolPolicy,
    /// Per-room overrides, keyed by channel and room id.
    pub rooms: HashMap<(ChannelId, String), RoomRule>,
    /// Fixed replies.
    pub replies: FixedReplies,
    /// IANA timezone for the time shown to the agent (`Asia/Ho_Chi_Minh`); UTC if unset.
    pub timezone: Option<String>,
    /// Continue the agent's native session between turns instead of resending the history.
    pub resume_sessions: bool,
    /// How often a "thinking…" message is updated with progress.
    pub progress_interval: Duration,
    /// Linked accounts (`[people]`): each account maps to its person.
    pub people: HashMap<(ChannelId, UserId), Person>,
    /// A personal assistant: only the owner's paired accounts are answered, and they share one
    /// profile and memory (person [`OWNER`]).
    pub personal: bool,
    /// Reminders.
    pub schedule: ScheduleConfig,
    /// Extra MCP servers (`[[mcp]]`), offered next to the knowledge servers.
    pub mcp: Vec<ExtraMcp>,
    /// Chat commands that call `[[mcp]]` tools directly (`[[command]]`).
    pub commands: Vec<McpCommand>,
}

/// An MCP server from the configuration and the bearer token it expects.
#[derive(Debug, Clone)]
pub struct ExtraMcp {
    /// Name, URL and the variable the agent reads the token from.
    pub server: McpServer,
    /// The token, passed to the agent process in that variable.
    pub token: Secret<String>,
    /// Offered only to `[bot] admins`.
    pub admins_only: bool,
    /// Give the agent a signed run token (who asked, where) instead of the secret itself; the
    /// server verifies it with the secret and may show prompts through `POST /interact`.
    pub signed: bool,
}

/// A chat command that calls one tool of an [`ExtraMcp`] server, without the agent.
#[derive(Debug, Clone, PartialEq)]
pub struct McpCommand {
    /// The command without `/`.
    pub name: String,
    /// One line for `/help` and command menus.
    pub description: String,
    /// The description in other languages: (language code, text).
    pub descriptions: Vec<(String, String)>,
    /// The [`ExtraMcp`] server name.
    pub mcp: String,
    /// The tool.
    pub tool: String,
    /// Positional arguments.
    pub args: Vec<CommandArg>,
    /// Arguments always passed.
    pub with: serde_json::Map<String, serde_json::Value>,
    /// Only `[bot] admins` may run it.
    pub admins_only: bool,
}

/// A positional argument of an [`McpCommand`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandArg {
    /// The tool argument it sets.
    pub name: String,
    /// Whether it may be left out, or takes the rest of the line.
    pub kind: ArgKind,
}

/// How a [`CommandArg`] is filled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgKind {
    /// One word, required.
    Required,
    /// One word, may be left out.
    Optional,
    /// The rest of the line, required.
    Rest,
}

impl CommandArg {
    /// Parses `name`, `name?` or `name...`.
    pub fn parse(spec: &str) -> Self {
        let (name, kind) = if let Some(n) = spec.strip_suffix("...") {
            (n, ArgKind::Rest)
        } else if let Some(n) = spec.strip_suffix('?') {
            (n, ArgKind::Optional)
        } else {
            (spec, ArgKind::Required)
        };
        Self {
            name: name.to_string(),
            kind,
        }
    }
}

impl McpCommand {
    /// The description in `locale`'s language (`ja`, `en-US`), or the default one.
    pub fn description_for(&self, locale: Option<&str>) -> &str {
        let lang = locale
            .and_then(|l| l.split(['-', '_']).next())
            .map(str::to_ascii_lowercase);
        lang.and_then(|l| self.descriptions.iter().find(|(code, _)| *code == l))
            .map_or(self.description.as_str(), |(_, text)| text.as_str())
    }

    /// `/name <a> [b] <c...>`.
    pub fn usage(&self) -> String {
        let mut out = format!("/{}", self.name);
        for a in &self.args {
            match a.kind {
                ArgKind::Required => {
                    let _ = write!(out, " <{}>", a.name);
                }
                ArgKind::Optional => {
                    let _ = write!(out, " [{}]", a.name);
                }
                ArgKind::Rest => {
                    let _ = write!(out, " <{}…>", a.name);
                }
            }
        }
        out
    }
}

/// When and where reminders are sent (design §5.6).
#[derive(Debug, Clone)]
pub struct ScheduleConfig {
    /// Send reminders.
    pub enabled: bool,
    /// The channel whose direct chat receives reminders that cannot go to their own chat.
    pub deliver_to: Option<ChannelId>,
    /// Firings found this late are still sent; older ones are marked missed.
    pub late_grace: Duration,
    /// Task files are re-read at least this often (hand edits, new tasks).
    pub rescan: Duration,
    /// Routine runs per local day, for the whole bot.
    pub max_agent_runs_per_day: u32,
}

impl Default for ScheduleConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            deliver_to: None,
            late_grace: Duration::from_secs(6 * 3600),
            rescan: Duration::from_secs(30),
            max_agent_runs_per_day: 24,
        }
    }
}

/// The person id of a personal bot's owner (`memory/people/owner/`).
pub const OWNER: &str = "owner";

impl RuntimeConfig {
    /// Links `accounts` as one person `id`.
    pub fn link_person(&mut self, id: &str, accounts: Vec<(ChannelId, UserId)>) {
        let person = Person {
            id: id.to_string(),
            accounts: accounts.clone(),
        };
        for account in accounts {
            self.people.insert(account, person.clone());
        }
    }

    /// The bot's time zone (`timezone`), UTC when unset or unknown.
    pub fn time_zone(&self) -> jiff::tz::TimeZone {
        self.timezone
            .as_deref()
            .and_then(|name| jiff::tz::TimeZone::get(name).ok())
            .unwrap_or(jiff::tz::TimeZone::UTC)
    }

    /// The person `user` of `channel` belongs to.
    pub fn person(&self, channel: &ChannelId, user: &UserId) -> Option<&Person> {
        self.people.get(&(channel.clone(), user.clone()))
    }

    /// Defaults with `work_dir` for agent working directories.
    pub fn new(bot_name: impl Into<String>, work_dir: impl Into<PathBuf>) -> Self {
        Self {
            bot_name: bot_name.into(),
            work_dir: work_dir.into(),
            agent_timeout: Duration::from_secs(180),
            history_messages: 40,
            history_tokens: 6000,
            dedupe_ttl: Duration::from_secs(24 * 3600),
            typing_interval: Duration::from_secs(8),
            worker_idle: Duration::from_secs(600),
            shutdown_grace: Duration::from_secs(30),
            access: AccessPolicy {
                max_input_chars: 8000,
                ..AccessPolicy::default()
            },
            tool_policy: ToolPolicy::default(),
            rooms: HashMap::new(),
            replies: FixedReplies::default(),
            timezone: None,
            resume_sessions: true,
            progress_interval: Duration::from_millis(1500),
            people: HashMap::new(),
            personal: false,
            schedule: ScheduleConfig::default(),
            mcp: Vec::new(),
            commands: Vec::new(),
        }
    }

    /// The rule for a room (defaults when none is configured).
    pub fn room(&self, channel: &ChannelId, room: &str) -> RoomRule {
        self.rooms
            .get(&(channel.clone(), room.to_string()))
            .copied()
            .unwrap_or_default()
    }
}
