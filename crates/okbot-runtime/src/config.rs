//! Runtime settings. `okbot-cli` builds these from `okbot.toml`.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use okbot_core::{ChannelId, Person, ToolPolicy, UserId};

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
}

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
