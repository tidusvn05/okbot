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
    AgentSection, BotProfile, BotSection, BridgeSection, ChannelAccess, ChannelsSection, Delivery,
    DiscordSection, GChatDelivery, GChatSection, HttpSection, McpSection, PersonSection,
    ProviderSection, ReplyOn, RoomSection, ScheduleSection, SlackSection, TelegramSection,
    ZaloSection,
};
pub use write::{
    GChatInit, InitOptions, ZaloInit, remove_env, render_channels, render_http, render_initial,
    upsert_env,
};

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
pub const BUILT_IN_PLATFORMS: &[&str] =
    &["console", "zalo", "gchat", "discord", "slack", "telegram"];

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
        let file: schema::File = toml::from_str(&text).map_err(|e| ConfigError::Parse {
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
            dotenv,
        };
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        let invalid = |m: String| Err(ConfigError::Invalid(m));
        self.validate_people()?;
        self.validate_schedule()?;
        self.validate_mcp()?;
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
            if !self.agent.chain.contains(name) {
                return invalid(format!("[agent.providers.{name}] is not in [agent] chain"));
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
            if matches!(m.name.as_str(), "kb" | "bot") {
                return invalid(format!("[[mcp]] name `{}` is reserved", m.name));
            }
            if !seen.insert(m.name.as_str()) {
                return invalid(format!("[[mcp]] name `{}` is used twice", m.name));
            }
            if !(m.url.starts_with("http://") || m.url.starts_with("https://")) {
                return invalid(format!(
                    "[[mcp]] `{}`: url must start with http:// or https://",
                    m.name
                ));
            }
            let plain = !m.token_env.is_empty()
                && !m.token_env.starts_with(|c: char| c.is_ascii_digit())
                && m.token_env
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_');
            if !plain {
                return invalid(format!(
                    "[[mcp]] `{}`: token_env must be a variable name such as OPS_MCP_TOKEN",
                    m.name
                ));
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
        names.extend(self.mcp.iter().map(|m| m.token_env.clone()));
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
