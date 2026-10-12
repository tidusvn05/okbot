//! Configuration schema, loading and validation for okbot bots.
//!
//! A bot is a directory (design §6.5) holding `okbot.toml`, an optional `.env` with secrets,
//! the knowledge bundle and the state directory. This crate reads and checks `okbot.toml`,
//! resolves secrets from the process environment and `.env`, and writes the file for
//! `okbot init`. It knows every platform's section, so a typo is an error, not a silent default.

mod dotenv;
mod schema;
mod write;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use okbot_core::Secret;

pub use schema::{
    AgentSection, BotSection, ChannelAccess, ChannelsSection, Delivery, GChatDelivery,
    GChatSection, HttpSection, ProviderSection, ReplyOn, RoomSection, ZaloSection,
};
pub use write::{GChatInit, InitOptions, ZaloInit, render_initial, upsert_env};

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
            dotenv,
        };
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        let invalid = |m: String| Err(ConfigError::Invalid(m));
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
        names.sort();
        names.dedup();
        names
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
