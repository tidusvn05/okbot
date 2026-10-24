//! Configuration schema, loading and validation for okbot bots.
//!
//! A bot is a directory (design §6.5) holding `okbot.toml`, an optional `.env` with secrets,
//! the knowledge bundle and the state directory. This crate reads and checks `okbot.toml`,
//! resolves secrets from the process environment and `.env`, and writes the file for
//! `okbot init`. It knows every platform's section, so a typo is an error, not a silent default.

mod dotenv;
mod edit;
mod schema;
mod write;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use okbot_core::Secret;

pub use edit::ConfigEditor;
pub use schema::{
    AgentSection, AutomationSection, BotProfile, BotSection, BridgeSection, ChannelAccess,
    ChannelsSection, CommandSection, Confirm, ConfirmBy, Delivery, DiscordSection, GChatDelivery,
    GChatSection, HttpSection, LimitsSection, McpAuth, McpCredentials, McpSection, NotifySection,
    OAuthSection, PersonSection, ProviderSection, ReplyOn, RestrictSection, RoomSection,
    ScheduleSection, SlackSection, SourceSection, TeamSection, TelegramSection, ZaloSection,
};
pub use write::{
    GChatInit, InitOptions, ZaloInit, remove_env, render_channels, render_http, render_initial,
    upsert_env,
};

/// Commands okbot handles itself; `[[command]]` cannot reuse them.
pub const BUILTIN_COMMANDS: &[&str] = &[
    "help",
    "reset",
    "new",
    "clear",
    "whoami",
    "stop",
    "cancel",
    "remember",
    "forget",
    "memories",
    "memory",
    "pair",
    "tasks",
    "todo",
    "todos",
    "done",
    "snooze",
    "later",
    "resume",
    "start",
    "pause",
    "note",
    "approve",
    "deny",
    "answer",
    "grant",
    "revoke",
    "roles",
    "connect",
    "disconnect",
    "connections",
];

/// Roles every bot has: `[bot] admins` (and the owner of a personal bot), and everyone it answers.
pub const BUILTIN_ROLES: &[&str] = &["admins", "everyone"];

/// The file name of a bot's configuration.
pub const CONFIG_FILE: &str = "okbot.toml";
/// The file name of a bot's secrets.
pub const ENV_FILE: &str = ".env";
/// The configuration format this build reads and writes.
pub const CONFIG_VERSION: u32 = 1;

/// Reading or checking the configuration failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// The file could not be read.
    #[error("cannot read {path}: {source}")]
    Read {
        /// File.
        path: PathBuf,
        /// Cause.
        source: std::io::Error,
    },
    /// The file is not valid TOML for this schema.
    #[error("{path} is invalid: {message}")]
    Parse {
        /// File.
        path: PathBuf,
        /// What is wrong, with the location.
        message: String,
    },
    /// The file was written for another configuration version.
    #[error("{path} has config_version {found}; this okbot reads version {CONFIG_VERSION}")]
    Version {
        /// File.
        path: PathBuf,
        /// Version in the file.
        found: u32,
    },
    /// A value is not acceptable.
    #[error("{0}")]
    Invalid(String),
    /// A secret is not set.
    #[error("{name} is not set: add it to {env_file} or the environment")]
    MissingSecret {
        /// Variable name.
        name: String,
        /// The bot's `.env`.
        env_file: PathBuf,
    },
}

/// Platforms with a native adapter; bridge adapters must use other names.
pub const BUILT_IN_PLATFORMS: &[&str] = &[
    "console", "zalo", "gchat", "discord", "slack", "telegram", "peer",
];

/// Parses an account written `platform:instance/user-id` (`zalo:shop/6ede9afa…`), as `/whoami`
/// shows it and `[bot] admins` and `[people]` use it.
pub fn parse_account(value: &str) -> Result<(okbot_core::ChannelId, okbot_core::UserId), String> {
    let (channel, user) = value
        .split_once('/')
        .filter(|(_, user)| !user.is_empty())
        .ok_or_else(|| format!("`{value}` must look like `zalo:shop/<user id>`"))?;
    let channel = channel.parse().map_err(|e| format!("`{value}`: {e}"))?;
    Ok((channel, okbot_core::UserId::new(user)))
}

/// Checks an `auth = "oauth"` server.
fn check_oauth(what: &str, m: &McpSection) -> Result<(), ConfigError> {
    let invalid = |m: String| Err(ConfigError::Invalid(m));
    if !(m.url.starts_with("http://") || m.url.starts_with("https://")) {
        return invalid(format!("{what}: url must start with http:// or https://"));
    }
    if !m.token_env.is_empty() {
        return invalid(format!(
            "{what}: an oauth server takes no token_env (people connect with /connect)"
        ));
    }
    if !m.env.is_empty() {
        return invalid(format!("{what}: env is for command servers"));
    }
    let Some(o) = &m.oauth else {
        return Ok(());
    };
    if o.client_id.as_deref().is_some_and(|c| c.trim().is_empty()) {
        return invalid(format!("{what}: [mcp.oauth] client_id is empty"));
    }
    if let Some(env) = &o.client_secret_env {
        if !is_env_name(env) {
            return invalid(format!(
                "{what}: [mcp.oauth] client_secret_env `{env}` is not a variable name"
            ));
        }
        if o.client_id.is_none() {
            return invalid(format!(
                "{what}: [mcp.oauth] client_secret_env needs client_id"
            ));
        }
    }
    if let Some(bad) = o
        .scopes
        .iter()
        .find(|s| s.is_empty() || s.contains(char::is_whitespace))
    {
        return invalid(format!("{what}: [mcp.oauth] scope `{bad}` is not a scope"));
    }
    Ok(())
}

/// A polling interval: `30m`, `2h`, `1d` (1 minute to 7 days; bare numbers are minutes).
pub fn parse_every(text: &str) -> Result<std::time::Duration, String> {
    let t = text.trim();
    let digits = t.find(|c: char| !c.is_ascii_digit()).unwrap_or(t.len());
    let (n, unit) = t.split_at(digits);
    let n: u64 = n
        .parse()
        .map_err(|_| format!("every `{text}` must look like 5m, 2h or 1d"))?;
    let minutes = match unit.trim() {
        "" | "m" | "min" => Some(n),
        "h" => n.checked_mul(60),
        "d" => n.checked_mul(24 * 60),
        _ => None,
    }
    .ok_or_else(|| format!("every `{text}` must look like 5m, 2h or 1d"))?;
    if !(1..=7 * 24 * 60).contains(&minutes) {
        return Err(format!("every `{text}` must be 1 minute to 7 days"));
    }
    Ok(std::time::Duration::from_secs(minutes * 60))
}

/// An environment variable name as okbot writes them (`OPS_MCP_TOKEN`).
fn is_env_name(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with(|c: char| c.is_ascii_digit())
        && value
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

/// A tool name or a pattern with `*` (any characters).
fn check_pattern(what: &str, pattern: &str) -> Result<(), ConfigError> {
    let ok = !pattern.is_empty()
        && pattern
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'*'));
    if ok {
        Ok(())
    } else {
        Err(ConfigError::Invalid(format!(
            "{what}: `{pattern}` is not a tool name or pattern (letters, digits, `_`, `-`, `.`, `*`)"
        )))
    }
}

fn is_slug(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// A loaded, validated bot configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// The bot directory.
    pub dir: PathBuf,
    /// `[bot]`.
    pub bot: BotSection,
    /// `[agent]`.
    pub agent: AgentSection,
    /// `[http]`.
    pub http: HttpSection,
    /// `[channels.*]`.
    pub channels: ChannelsSection,
    /// `[people.*]`: linked accounts by person id.
    pub people: BTreeMap<String, PersonSection>,
    /// `[schedule]`.
    pub schedule: ScheduleSection,
    /// `[[mcp]]`: extra MCP servers for the agent.
    pub mcp: Vec<McpSection>,
    /// `[[command]]`: chat commands that call `[[mcp]]` tools directly.
    pub commands: Vec<CommandSection>,
    /// `[[notify]]`: named destinations other programs post messages to.
    pub notify: Vec<NotifySection>,
    /// `[roles]`: members (accounts, or `person:<id>`) by role name.
    pub roles: BTreeMap<String, Vec<String>>,
    /// `[limits]`.
    pub limits: LimitsSection,
    /// `[[automation]]`: triggers, sources and actions (ADR 0013).
    pub automations: Vec<AutomationSection>,
    /// `[teams.<id>]`: teams of okbots this bot belongs to (ADR 0014).
    pub teams: BTreeMap<String, TeamSection>,
    /// Values from `.env`, wrapped so `Debug` never shows them.
    dotenv: HashMap<String, Secret<String>>,
}

impl Config {
    /// Loads `dir/okbot.toml` and `dir/.env`.
    pub fn load(dir: &Path) -> Result<Self, ConfigError> {
        let path = dir.join(CONFIG_FILE);
        let text = std::fs::read_to_string(&path).map_err(|source| ConfigError::Read {
            path: path.clone(),
            source,
        })?;
        Self::from_text(dir, &text)
    }

    /// Parses candidate configuration text without writing it; resolves `.env` from `dir`.
    pub fn from_text(dir: &Path, text: &str) -> Result<Self, ConfigError> {
        let path = dir.join(CONFIG_FILE);
        let file: schema::File = toml::from_str(text).map_err(|e| ConfigError::Parse {
            path: path.clone(),
            message: e.to_string(),
        })?;
        if file.config_version != CONFIG_VERSION {
            return Err(ConfigError::Version {
                path,
                found: file.config_version,
            });
        }
        let env_path = dir.join(ENV_FILE);
        let dotenv = match std::fs::read_to_string(&env_path) {
            Ok(text) => dotenv::parse(&text)
                .into_iter()
                .map(|(k, v)| (k, Secret::new(v)))
                .collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(source) => {
                return Err(ConfigError::Read {
                    path: env_path,
                    source,
                });
            }
        };
        let config = Self {
            dir: dir.to_path_buf(),
            bot: file.bot,
            agent: file.agent,
            http: file.http,
            channels: file.channels,
            people: file.people,
            schedule: file.schedule,
            mcp: file.mcp,
            commands: file.command,
            notify: file.notify,
            roles: file.roles,
            limits: file.limits,
            automations: file.automation,
            teams: file.teams,
            dotenv,
        };
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        let invalid = |m: String| Err(ConfigError::Invalid(m));
        self.validate_people()?;
        self.validate_roles()?;
        self.validate_schedule()?;
        self.validate_mcp()?;
        self.validate_commands()?;
        self.validate_notify()?;
        self.validate_automations()?;
        self.validate_teams()?;
        self.validate_http()?;
        if self.bot.name.trim().is_empty() {
            return invalid("[bot] name must not be empty".into());
        }
        if self.agent.chain.is_empty() {
            return invalid("[agent] chain must name at least one agent CLI".into());
        }
        for name in &self.agent.chain {
            if !matches!(name.as_str(), "claude" | "codex" | "opencode" | "devin") {
                return invalid(format!(
                    "[agent] chain: unknown agent `{name}` (use claude, codex, opencode or devin)"
                ));
            }
        }
        for name in self.agent.providers.keys() {
            if !matches!(name.as_str(), "claude" | "codex" | "opencode" | "devin") {
                return invalid(format!("[agent.providers.{name}]: unknown provider"));
            }
        }
        if self.agent.max_concurrent == 0 {
            return invalid("[agent] max_concurrent must be at least 1".into());
        }
        for (instance, zalo) in &self.channels.zalo {
            okbot_core::ChannelId::new("zalo", instance)
                .map_err(|e| ConfigError::Invalid(format!("[channels.zalo.{instance}]: {e}")))?;
            if let Delivery::Webhook = zalo.delivery {
                let url = zalo.public_url.as_deref().unwrap_or_default();
                if !url.starts_with("https://") {
                    return invalid(format!(
                        "[channels.zalo.{instance}] webhook delivery needs public_url = \"https://…\""
                    ));
                }
                if zalo.secret_env.is_none() {
                    return invalid(format!(
                        "[channels.zalo.{instance}] webhook delivery needs secret_env"
                    ));
                }
            }
        }
        for (instance, t) in &self.channels.telegram {
            if let Delivery::Webhook = t.delivery {
                let url = t.public_url.as_deref().unwrap_or_default();
                if !url.starts_with("https://") {
                    return invalid(format!(
                        "[channels.telegram.{instance}] webhook delivery needs public_url = \"https://…\""
                    ));
                }
                if t.secret_env.is_none() {
                    return invalid(format!(
                        "[channels.telegram.{instance}] webhook delivery needs secret_env"
                    ));
                }
            }
        }
        for (instance, b) in &self.channels.bridge {
            if BUILT_IN_PLATFORMS.contains(&b.platform.as_str()) {
                return invalid(format!(
                    "[channels.bridge.{instance}] platform \"{}\" is built in; use [channels.{}.{instance}]",
                    b.platform, b.platform
                ));
            }
        }
        for (platform, instance, _) in self.channels.access() {
            okbot_core::ChannelId::new(platform, instance).map_err(|e| {
                ConfigError::Invalid(format!("[channels.{platform}.{instance}]: {e}"))
            })?;
        }
        for (instance, g) in &self.channels.gchat {
            okbot_core::ChannelId::new("gchat", instance)
                .map_err(|e| ConfigError::Invalid(format!("[channels.gchat.{instance}]: {e}")))?;
            match g.delivery {
                GChatDelivery::Pubsub
                    if g.subscription
                        .as_deref()
                        .is_none_or(|s| !s.starts_with("projects/")) =>
                {
                    return invalid(format!(
                        "[channels.gchat.{instance}] pubsub delivery needs subscription = \"projects/<p>/subscriptions/<s>\""
                    ));
                }
                GChatDelivery::Http
                    if g.audience_url.is_none() == g.audience_project_number.is_none() =>
                {
                    return invalid(format!(
                        "[channels.gchat.{instance}] http delivery needs exactly one of audience_url or audience_project_number"
                    ));
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn validate_http(&self) -> Result<(), ConfigError> {
        let invalid = |m: String| Err(ConfigError::Invalid(m));
        if let Some(url) = &self.http.public_url
            && (!url.starts_with("https://") || url.ends_with('/'))
        {
            return invalid(format!(
                "[http] public_url `{url}` must start with https:// and not end with `/`"
            ));
        }
        if self.http.public_url.is_some() && self.http.listen.is_none() {
            return invalid("[http] public_url needs [http] listen".into());
        }
        Ok(())
    }

    fn validate_teams(&self) -> Result<(), ConfigError> {
        let invalid = |m: String| Err(ConfigError::Invalid(m));
        for (id, team) in &self.teams {
            if !is_slug(id) {
                return invalid(format!(
                    "[teams.{id}]: the id may only contain a-z, 0-9, `-` and `_`"
                ));
            }
            if !team.member.is_empty() && !is_slug(&team.member) {
                return invalid(format!(
                    "[teams.{id}] member `{}` may only contain a-z, 0-9, `-` and `_`",
                    team.member
                ));
            }
            if team.manifest.starts_with("http://") {
                return invalid(format!("[teams.{id}] manifest must be https:// or a file"));
            }
            if team.manifest.starts_with("https://") && team.team_key.is_empty() {
                return invalid(format!(
                    "[teams.{id}] a manifest fetched over the network needs `team_key`"
                ));
            }
            if !team.team_key.is_empty() && !team.team_key.starts_with("ed25519:") {
                return invalid(format!(
                    "[teams.{id}] team_key must be `ed25519:<base64url>`"
                ));
            }
            for (team_role, roles) in &team.roles {
                if roles.iter().any(|r| r == "admins") {
                    return invalid(format!(
                        "[teams.{id}] roles.{team_role}: `admins` is never given to another bot"
                    ));
                }
                self.check_roles(&format!("[teams.{id}] roles.{team_role}"), roles)?;
            }
            self.check_roles(
                &format!("[teams.{id}] delegate_roles"),
                &team.delegate_roles,
            )?;
            if team.confirm == Confirm::Destructive {
                return invalid(format!("[teams.{id}] confirm is `never` or `always`"));
            }
            if team.max_depth == 0 || team.max_depth > 8 {
                return invalid(format!("[teams.{id}] max_depth must be 1 to 8"));
            }
            if team.max_concurrent == 0 {
                return invalid(format!("[teams.{id}] max_concurrent must be at least 1"));
            }
            if team.wait > 600 || team.task_timeout < 30 || team.task_timeout > 86_400 {
                return invalid(format!(
                    "[teams.{id}] wait is at most 600 s; task_timeout is 30 s to 1 day"
                ));
            }
        }
        Ok(())
    }

    fn validate_people(&self) -> Result<(), ConfigError> {
        let invalid = |m: String| Err(ConfigError::Invalid(m));
        let mut linked = HashMap::new();
        for (id, person) in &self.people {
            if !is_slug(id) {
                return invalid(format!(
                    "[people.{id}]: the id may only contain a-z, 0-9, `-` and `_`"
                ));
            }
            for account in &person.accounts {
                let key = parse_account(account)
                    .map_err(|e| ConfigError::Invalid(format!("[people.{id}] accounts: {e}")))?;
                if let Some(other) = linked.insert(key, id) {
                    return invalid(format!(
                        "account `{account}` is linked to both `{other}` and `{id}` in [people]"
                    ));
                }
            }
        }
        Ok(())
    }

    fn validate_roles(&self) -> Result<(), ConfigError> {
        let invalid = |m: String| Err(ConfigError::Invalid(m));
        for (role, members) in &self.roles {
            if !is_slug(role) {
                return invalid(format!(
                    "[roles] `{role}`: a role name may only contain a-z, 0-9, `-` and `_`"
                ));
            }
            if BUILTIN_ROLES.contains(&role.as_str()) {
                return invalid(format!("[roles] `{role}` is built in"));
            }
            for member in members {
                if let Some(person) = member.strip_prefix("person:") {
                    if !self.people.contains_key(person) {
                        return invalid(format!("[roles] {role}: `{member}` is not in [people]"));
                    }
                } else {
                    parse_account(member)
                        .map_err(|e| ConfigError::Invalid(format!("[roles] {role}: {e}")))?;
                }
            }
        }
        Ok(())
    }

    /// Checks role names used by `[[mcp]]` or `[[command]]`.
    fn check_roles(&self, what: &str, roles: &[String]) -> Result<(), ConfigError> {
        for role in roles {
            if !BUILTIN_ROLES.contains(&role.as_str()) && !self.roles.contains_key(role) {
                return Err(ConfigError::Invalid(format!(
                    "{what}: role `{role}` is not in [roles] (built in: admins, everyone)"
                )));
            }
        }
        Ok(())
    }

    fn validate_schedule(&self) -> Result<(), ConfigError> {
        let invalid = |m: String| Err(ConfigError::Invalid(m));
        if let Some(channel) = &self.schedule.deliver_to {
            channel.parse::<okbot_core::ChannelId>().map_err(|e| {
                ConfigError::Invalid(format!("[schedule] deliver_to `{channel}`: {e}"))
            })?;
        }
        if let Some(tz) = &self.bot.timezone
            && jiff::tz::TimeZone::get(tz).is_err()
        {
            return invalid(format!(
                "[bot] timezone `{tz}` is not an IANA time zone (for example Asia/Ho_Chi_Minh)"
            ));
        }
        Ok(())
    }

    fn validate_mcp(&self) -> Result<(), ConfigError> {
        let invalid = |m: String| Err(ConfigError::Invalid(m));
        let mut seen = std::collections::HashSet::new();
        for m in &self.mcp {
            if !is_slug(&m.name) || m.name.contains('_') {
                return invalid(format!(
                    "[[mcp]] name `{}`: use a-z, 0-9 and `-` only",
                    m.name
                ));
            }
            if matches!(m.name.as_str(), "kb" | "bot" | "team") {
                return invalid(format!("[[mcp]] name `{}` is reserved", m.name));
            }
            if !seen.insert(m.name.as_str()) {
                return invalid(format!("[[mcp]] name `{}` is used twice", m.name));
            }
            let what = format!("[[mcp]] `{}`", m.name);
            if m.auth != McpAuth::Oauth
                && (m.credentials != McpCredentials::Person || m.oauth.is_some())
            {
                return invalid(format!(
                    "{what}: credentials and [mcp.oauth] are for auth = \"oauth\""
                ));
            }
            if m.command.is_empty() && m.auth == McpAuth::Oauth {
                check_oauth(&what, m)?;
            } else if m.command.is_empty() {
                if !(m.url.starts_with("http://") || m.url.starts_with("https://")) {
                    return invalid(format!(
                        "{what}: url must start with http:// or https:// (or give a command)"
                    ));
                }
                if !is_env_name(&m.token_env) {
                    return invalid(format!(
                        "{what}: token_env must be a variable name such as OPS_MCP_TOKEN"
                    ));
                }
                if !m.env.is_empty() {
                    return invalid(format!("{what}: env is for command servers"));
                }
            } else {
                if !m.url.is_empty() || !m.token_env.is_empty() || m.auth != McpAuth::Bearer {
                    return invalid(format!(
                        "{what}: a command server takes no url, token_env or auth"
                    ));
                }
                if m.command[0].trim().is_empty() {
                    return invalid(format!("{what}: command must start with a program"));
                }
                if let Some(bad) = m.env.iter().find(|v| !is_env_name(v)) {
                    return invalid(format!("{what}: env `{bad}` is not a variable name"));
                }
            }
            self.check_roles(&what, &m.roles)?;
            for room in &m.rooms {
                parse_account(room).map_err(|_| {
                    ConfigError::Invalid(format!(
                        "{what} rooms: `{room}` must look like `discord:main/<room id>`"
                    ))
                })?;
            }
            for pattern in m.tools.iter().chain(&m.deny_tools) {
                check_pattern(&what, pattern)?;
            }
            for r in &m.restrict {
                if r.tools.is_empty() || r.roles.is_empty() {
                    return invalid(format!("{what} restrict: needs tools and roles"));
                }
                r.tools.iter().try_for_each(|p| check_pattern(&what, p))?;
                self.check_roles(&format!("{what} restrict"), &r.roles)?;
            }
        }
        Ok(())
    }

    fn validate_commands(&self) -> Result<(), ConfigError> {
        let invalid = |m: String| Err(ConfigError::Invalid(m));
        let mut seen = std::collections::HashSet::new();
        for c in &self.commands {
            let n = &c.name;
            let plain = !n.is_empty()
                && n.len() <= 32
                && n.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
            if !plain {
                return invalid(format!(
                    "[[command]] name `{n}`: use a-z, 0-9 and `_` only, at most 32 (no `/`)"
                ));
            }
            if BUILTIN_COMMANDS.contains(&n.as_str()) {
                return invalid(format!("[[command]] `{n}` is a built-in command"));
            }
            if !seen.insert(n.as_str()) {
                return invalid(format!("[[command]] `{n}` is defined twice"));
            }
            if c.description.chars().count() > 100 || c.description.contains('\n') {
                return invalid(format!(
                    "[[command]] `{n}`: description must be one line of at most 100 characters"
                ));
            }
            for (code, text) in &c.descriptions {
                let code_ok =
                    (2..=3).contains(&code.len()) && code.bytes().all(|b| b.is_ascii_lowercase());
                if !code_ok
                    || text.chars().count() > 100
                    || text.contains('\n')
                    || text.trim().is_empty()
                {
                    return invalid(format!(
                        "[[command]] `{n}`: descriptions.{code}: a language code (en, ja) and one line of at most 100 characters"
                    ));
                }
            }
            self.check_roles(&format!("[[command]] `{n}`"), &c.roles)?;
            if !self.mcp.iter().any(|m| m.name == c.mcp) {
                return invalid(format!(
                    "[[command]] `{n}`: mcp `{}` is not an [[mcp]] server",
                    c.mcp
                ));
            }
            if c.tool.trim().is_empty() {
                return invalid(format!("[[command]] `{n}`: tool must not be empty"));
            }
            let mut optional = false;
            for (i, arg) in c.args.iter().enumerate() {
                let (base, kind) = if let Some(b) = arg.strip_suffix("...") {
                    (b, "rest")
                } else if let Some(b) = arg.strip_suffix('?') {
                    (b, "optional")
                } else {
                    (arg.as_str(), "required")
                };
                let ident = base.starts_with(|ch: char| ch.is_ascii_alphabetic() || ch == '_')
                    && base
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
                if !ident {
                    return invalid(format!("[[command]] `{n}`: argument `{arg}` is not a name"));
                }
                if c.with.contains_key(base) {
                    return invalid(format!(
                        "[[command]] `{n}`: `{base}` is fixed by `with`; it cannot also be an argument"
                    ));
                }
                if kind == "rest" && i + 1 != c.args.len() {
                    return invalid(format!(
                        "[[command]] `{n}`: only the last argument may take the rest (`{arg}`)"
                    ));
                }
                if kind == "required" && optional {
                    return invalid(format!(
                        "[[command]] `{n}`: required `{arg}` after an optional argument"
                    ));
                }
                optional |= kind == "optional";
            }
        }
        Ok(())
    }

    fn validate_notify(&self) -> Result<(), ConfigError> {
        let invalid = |m: String| Err(ConfigError::Invalid(m));
        if !self.notify.is_empty() && self.http.listen.is_none() {
            return invalid(
                "[[notify]] needs [http] listen = \"127.0.0.1:<port>\" (callers post to it)".into(),
            );
        }
        let mut seen = std::collections::HashSet::new();
        for n in &self.notify {
            let name = &n.name;
            if !is_slug(name) || name.len() > 64 {
                return invalid(format!(
                    "[[notify]] name `{name}`: use a-z, 0-9, `-` and `_` only, at most 64"
                ));
            }
            if !seen.insert(name.as_str()) {
                return invalid(format!("[[notify]] `{name}` is defined twice"));
            }
            if !is_env_name(&n.token_env) {
                return invalid(format!(
                    "[[notify]] `{name}`: token_env must be a variable name such as OKBOT_NOTIFY_TOKEN"
                ));
            }
            // The agent receives a bearer [[mcp]] token in its environment: it must not be able
            // to post notifications with it.
            if let Some(m) = self
                .mcp
                .iter()
                .find(|m| m.auth == McpAuth::Bearer && m.token_env == n.token_env)
            {
                return invalid(format!(
                    "[[notify]] `{name}`: token_env {} is the token agents get for [[mcp]] `{}`; use another variable",
                    n.token_env, m.name
                ));
            }
            if n.to.is_empty() {
                return invalid(format!("[[notify]] `{name}`: `to` names no target"));
            }
            self.check_targets(&format!("[[notify]] `{name}`"), &n.to)?;
        }
        Ok(())
    }

    fn validate_automations(&self) -> Result<(), ConfigError> {
        let mut seen = std::collections::HashSet::new();
        for a in &self.automations {
            let name = &a.name;
            if !is_slug(name) || name.len() > 64 {
                return Err(ConfigError::Invalid(format!(
                    "[[automation]] name `{name}`: use a-z, 0-9, `-` and `_` only, at most 64"
                )));
            }
            if !seen.insert(name.as_str()) {
                return Err(ConfigError::Invalid(format!(
                    "[[automation]] `{name}` is defined twice"
                )));
            }
            let what = format!("[[automation]] `{name}`");
            self.check_trigger(&what, a)?;
            self.check_action(&what, a)?;
            self.check_source(&what, a)?;
            if !(1..=100).contains(&a.max_items) {
                return Err(ConfigError::Invalid(format!(
                    "{what}: max_items must be 1 to 100"
                )));
            }
        }
        Ok(())
    }

    fn check_trigger(&self, what: &str, a: &AutomationSection) -> Result<(), ConfigError> {
        let invalid = |m: String| Err(ConfigError::Invalid(m));
        let triggers = usize::from(a.every.is_some())
            + usize::from(a.schedule.is_some())
            + usize::from(a.webhook);
        if triggers != 1 {
            return invalid(format!(
                "{what}: give exactly one trigger: every, schedule or webhook = true"
            ));
        }
        if let Some(every) = &a.every {
            parse_every(every).map_err(|e| ConfigError::Invalid(format!("{what}: {e}")))?;
            if a.source.is_none() {
                return invalid(format!(
                    "{what}: every polls a source; add [automation.source]"
                ));
            }
        }
        if let Some(rule) = &a.schedule {
            rule.parse::<okbot_core::Repeat>()
                .map_err(|e| ConfigError::Invalid(format!("{what}: schedule {e}")))?;
        }
        if !a.webhook {
            if !a.token_env.is_empty() {
                return invalid(format!("{what}: token_env is for webhooks"));
            }
            return Ok(());
        }
        if self.http.listen.is_none() {
            return invalid(format!(
                "{what}: a webhook needs [http] listen = \"127.0.0.1:<port>\""
            ));
        }
        if !is_env_name(&a.token_env) {
            return invalid(format!(
                "{what}: a webhook needs token_env, a variable such as OKBOT_HOOK_TOKEN"
            ));
        }
        if let Some(m) = self
            .mcp
            .iter()
            .find(|m| m.auth == McpAuth::Bearer && m.token_env == a.token_env)
        {
            return invalid(format!(
                "{what}: token_env {} is the token agents get for [[mcp]] `{}`; use another variable",
                a.token_env, m.name
            ));
        }
        if a.source.is_some() {
            return invalid(format!(
                "{what}: a webhook brings its own data; it takes no source"
            ));
        }
        Ok(())
    }

    fn check_action(&self, what: &str, a: &AutomationSection) -> Result<(), ConfigError> {
        let invalid = |m: String| Err(ConfigError::Invalid(m));
        match (&a.text, &a.prompt) {
            (Some(_), Some(_)) | (None, None) => {
                return invalid(format!("{what}: give either text or prompt"));
            }
            (Some(text), None) => {
                if text.trim().is_empty() {
                    return invalid(format!("{what}: text is empty"));
                }
                if a.to.is_empty() {
                    return invalid(format!("{what}: text needs `to`"));
                }
            }
            (None, Some(prompt)) => {
                if prompt.trim().is_empty() {
                    return invalid(format!("{what}: prompt is empty"));
                }
                if a.to.len() > 1 {
                    return invalid(format!(
                        "{what}: a prompt answers in one chat; `to` names more"
                    ));
                }
                if a.to.first().is_some_and(|t| t == "admins") {
                    return invalid(format!("{what}: a prompt cannot go to `admins`"));
                }
            }
        }
        self.check_targets(what, &a.to)?;
        let run_as = self.automation_run_as(a);
        if let Some(run_as) = &run_as {
            self.check_person_or_account(&format!("{what} run_as"), run_as)?;
            let personal = self.bot.profile == BotProfile::Personal;
            if personal && run_as != "person:owner" {
                return invalid(format!(
                    "{what}: a personal bot runs automations as its owner; use run_as = \"person:owner\" or leave it out"
                ));
            }
        }
        if a.prompt.is_some() && run_as.is_none() {
            return invalid(format!(
                "{what}: a prompt needs run_as, the person the agent works for"
            ));
        }
        Ok(())
    }

    fn check_source(&self, what: &str, a: &AutomationSection) -> Result<(), ConfigError> {
        let invalid = |m: String| Err(ConfigError::Invalid(m));
        let Some(source) = &a.source else {
            return Ok(());
        };
        let Some(m) = self.mcp.iter().find(|m| m.name == source.mcp) else {
            return invalid(format!(
                "{what}: source mcp `{}` is not an [[mcp]] server",
                source.mcp
            ));
        };
        if !m.command.is_empty() {
            return invalid(format!(
                "{what}: source mcp `{}` is a command server; use an HTTP one",
                source.mcp
            ));
        }
        let run_as = self.automation_run_as(a);
        let needs_person = (m.auth == McpAuth::Oauth && m.credentials == McpCredentials::Person)
            || m.auth == McpAuth::Signed;
        if needs_person && run_as.is_none() {
            return invalid(format!(
                "{what}: `{}` acts for a person; set run_as",
                source.mcp
            ));
        }
        if m.auth == McpAuth::Oauth
            && m.credentials == McpCredentials::Person
            && let Some(bad) =
                a.to.iter()
                    .find(|t| !self.private_target(t, run_as.as_deref()))
        {
            return invalid(format!(
                "{what}: `{}` reads run_as's own account, so it may only go to them or a \
                 `personal` room, not `{bad}`",
                source.mcp
            ));
        }
        if source.tool.trim().is_empty() {
            return invalid(format!("{what}: source tool is empty"));
        }
        for (key, pointer) in [("items", &source.items), ("id", &source.id)] {
            if let Some(p) = pointer
                && !p.starts_with('/')
            {
                return invalid(format!(
                    "{what}: source {key} `{p}` must be a JSON pointer such as /messages"
                ));
            }
        }
        if source.id.is_some() && source.items.is_none() {
            return invalid(format!("{what}: source id needs items"));
        }
        Ok(())
    }

    /// `target` is read only by `run_as`: their own direct chat (as a person or one of their
    /// accounts), or a room marked `personal`.
    fn private_target(&self, target: &str, run_as: Option<&str>) -> bool {
        let Ok(parsed) = target.parse::<okbot_core::NotifyTo>() else {
            return false;
        };
        let run_as_person = run_as.and_then(|r| r.strip_prefix("person:"));
        match parsed {
            okbot_core::NotifyTo::Person(id) => run_as_person == Some(id.as_str()),
            okbot_core::NotifyTo::User(channel, user) => {
                let account = format!("{channel}/{user}");
                run_as == Some(account.as_str())
                    || run_as_person
                        .and_then(|p| self.people.get(p))
                        .is_some_and(|p| p.accounts.contains(&account))
            }
            okbot_core::NotifyTo::Room(key) => self.channels.access().iter().any(|(p, i, a)| {
                key.channel.platform() == *p
                    && key.channel.instance() == *i
                    && a.rooms.iter().any(|r| r.room == key.room && r.personal)
            }),
            _ => false,
        }
    }

    /// Whose an automation runs as: its `run_as`, else the owner on a personal bot.
    pub fn automation_run_as(&self, a: &AutomationSection) -> Option<String> {
        a.run_as.clone().or_else(|| {
            (self.bot.profile == BotProfile::Personal).then(|| "person:owner".to_string())
        })
    }

    /// `person:<id>` of a linked person (or the owner of a personal bot), or an account.
    fn check_person_or_account(&self, what: &str, value: &str) -> Result<(), ConfigError> {
        if let Some(id) = value.strip_prefix("person:") {
            let owner = id == "owner" && self.bot.profile == BotProfile::Personal;
            if owner || self.people.contains_key(id) {
                return Ok(());
            }
            return Err(ConfigError::Invalid(format!(
                "{what}: `{value}` is not in [people] (`okbot people link`)"
            )));
        }
        parse_account(value)
            .map(drop)
            .map_err(|e| ConfigError::Invalid(format!("{what}: {e}")))
    }

    /// Checks `[[notify]]`-style targets.
    fn check_targets(&self, what: &str, targets: &[String]) -> Result<(), ConfigError> {
        let channels: Vec<String> = self
            .channels
            .access()
            .iter()
            .map(|(platform, instance, _)| format!("{platform}:{instance}"))
            .collect();
        for target in targets {
            let parsed: okbot_core::NotifyTo = target
                .parse()
                .map_err(|e| ConfigError::Invalid(format!("{what}: {e}")))?;
            let channel = match &parsed {
                okbot_core::NotifyTo::Room(key) => Some(key.channel.to_string()),
                okbot_core::NotifyTo::User(channel, _) => Some(channel.to_string()),
                okbot_core::NotifyTo::Person(id) => {
                    let owner = id == "owner" && self.bot.profile == BotProfile::Personal;
                    if !owner && !self.people.contains_key(id) {
                        return Err(ConfigError::Invalid(format!(
                            "{what}: `{target}` is not in [people] (`okbot people link`)"
                        )));
                    }
                    None
                }
                _ => None,
            };
            if let Some(channel) = channel
                && !channels.contains(&channel)
            {
                return Err(ConfigError::Invalid(format!(
                    "{what}: `{target}` is on {channel}, which is not in [channels]"
                )));
            }
        }
        Ok(())
    }

    /// A secret by variable name: the process environment first, then the bot's `.env`.
    pub fn secret(&self, name: &str) -> Result<Secret<String>, ConfigError> {
        std::env::var(name)
            .ok()
            .or_else(|| self.dotenv.get(name).map(|v| v.expose().clone()))
            .filter(|v| !v.is_empty())
            .map(Secret::new)
            .ok_or_else(|| ConfigError::MissingSecret {
                name: name.to_string(),
                env_file: self.dir.join(ENV_FILE),
            })
    }

    /// Every secret variable name the configuration refers to (kept away from agents).
    pub fn secret_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.dotenv.keys().cloned().collect();
        for zalo in self.channels.zalo.values() {
            names.push(zalo.token_env.clone());
            names.extend(zalo.secret_env.clone());
        }
        for d in self.channels.discord.values() {
            names.push(d.token_env.clone());
        }
        for s in self.channels.slack.values() {
            names.push(s.app_token_env.clone());
            names.push(s.bot_token_env.clone());
        }
        for t in self.channels.telegram.values() {
            names.push(t.token_env.clone());
            names.extend(t.secret_env.clone());
        }
        for b in self.channels.bridge.values() {
            names.extend(b.env.iter().cloned());
        }
        names.extend(self.notify.iter().map(|n| n.token_env.clone()));
        for m in &self.mcp {
            if !m.token_env.is_empty() {
                names.push(m.token_env.clone());
            }
            names.extend(m.env.iter().cloned());
            names.extend(m.oauth.iter().filter_map(|o| o.client_secret_env.clone()));
        }
        names.extend(
            self.automations
                .iter()
                .filter(|a| !a.token_env.is_empty())
                .map(|a| a.token_env.clone()),
        );
        names.sort();
        names.dedup();
        names
    }

    /// Every channel as `(id, section kind, secret variable names)`; the kind is the
    /// `[channels.<kind>]` table (`bridge` for bridge adapters).
    pub fn channel_secrets(&self) -> Vec<(String, &'static str, Vec<String>)> {
        let c = &self.channels;
        let mut out = Vec::new();
        for (i, z) in &c.zalo {
            let mut names = vec![z.token_env.clone()];
            names.extend(z.secret_env.clone());
            out.push((format!("zalo:{i}"), "zalo", names));
        }
        for i in c.gchat.keys() {
            out.push((format!("gchat:{i}"), "gchat", Vec::new()));
        }
        for (i, d) in &c.discord {
            out.push((format!("discord:{i}"), "discord", vec![d.token_env.clone()]));
        }
        for (i, s) in &c.slack {
            out.push((
                format!("slack:{i}"),
                "slack",
                vec![s.app_token_env.clone(), s.bot_token_env.clone()],
            ));
        }
        for (i, t) in &c.telegram {
            let mut names = vec![t.token_env.clone()];
            names.extend(t.secret_env.clone());
            out.push((format!("telegram:{i}"), "telegram", names));
        }
        for (i, b) in &c.bridge {
            out.push((format!("{}:{i}", b.platform), "bridge", b.env.clone()));
        }
        out
    }

    /// A path from the configuration, relative to the bot directory.
    pub fn path(&self, relative: &Path) -> PathBuf {
        if relative.is_absolute() {
            relative.to_path_buf()
        } else {
            self.dir.join(relative)
        }
    }

    /// The knowledge bundle directory.
    pub fn kb_dir(&self) -> PathBuf {
        self.path(&self.bot.kb)
    }

    /// The state directory.
    pub fn state_dir(&self) -> PathBuf {
        self.path(&self.bot.state_dir)
    }
}

#[cfg(test)]
mod tests;
