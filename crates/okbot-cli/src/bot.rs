//! The composition root: turns a bot's `okbot.toml` into a running pipeline.
//!
//! This is the only place that knows every adapter crate (ADR 0001).

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use okbot_agent::{AgentChain, CliBackend, CliBackendConfig, EnvScrub};
use okbot_config::{Config, ReplyOn as ConfigReplyOn};
use okbot_core::{
    AgentBackend, CancellationToken, Channel, ChannelId, ConversationStore, KnowledgeProvider,
    UserId,
};
use okbot_knowledge::{Knowledge, KnowledgeOptions};
use okbot_render::MarkdownRenderer;
use okbot_runtime::config::{ReplyOn, RoomRule};
use okbot_runtime::{HttpOptions, Runtime, RuntimeConfig};
use okbot_store::SqliteStore;

/// Everything `okbot run` starts.
pub(crate) struct Bot {
    pub(crate) runtime: Runtime,
    pub(crate) store: Arc<SqliteStore>,
    pub(crate) retention_days: u32,
}

fn reply_on(values: &[ConfigReplyOn]) -> ReplyOn {
    ReplyOn {
        direct: values.contains(&ConfigReplyOn::Dm),
        mention: values.contains(&ConfigReplyOn::Mention),
        all: values.contains(&ConfigReplyOn::All),
    }
}

/// `platform:instance/user` → (channel, user).
fn parse_admin(value: &str) -> anyhow::Result<(ChannelId, UserId)> {
    okbot_config::parse_account(value).map_err(|e| anyhow::anyhow!("[bot] admins: {e}"))
}

/// Environment for the agent CLIs when `[agent] home` is set: a separate login and quota.
pub(crate) fn agent_env(config: &Config) -> Vec<(String, String)> {
    let Some(home) = &config.agent.home else {
        return Vec::new();
    };
    let home = config.path(home);
    let home = home.to_string_lossy();
    vec![
        ("HOME".into(), home.to_string()),
        ("CLAUDE_CONFIG_DIR".into(), format!("{home}/.claude")),
        ("CODEX_HOME".into(), format!("{home}/.codex")),
    ]
}

/// The machine-wide agent limit from `~/.config/okbot/config.toml` (`[limits] max_agents`).
fn machine_slots() -> anyhow::Result<Option<okbot_agent::MachineSlots>> {
    #[derive(serde::Deserialize, Default)]
    struct Machine {
        #[serde(default)]
        limits: Limits,
    }
    #[derive(serde::Deserialize, Default)]
    struct Limits {
        max_agents: Option<usize>,
    }
    let home = crate::registry::home()?;
    let path = home.join("config.toml");
    let machine: Machine = match std::fs::read_to_string(&path) {
        Ok(text) => {
            toml::from_str(&text).with_context(|| format!("{} is invalid", path.display()))?
        }
        Err(_) => Machine::default(),
    };
    Ok(machine
        .limits
        .max_agents
        .map(|max| okbot_agent::MachineSlots::new(home.join("slots"), max)))
}

/// The agent chain from `[agent]`.
pub(crate) fn agent_chain(config: &Config) -> anyhow::Result<AgentChain> {
    agent_chain_with_sessions(config, true)
}

/// An isolated operator probe uses the usual policy/limits without retaining native sessions.
pub(crate) fn agent_probe_chain(config: &Config) -> anyhow::Result<AgentChain> {
    agent_chain_with_sessions(config, false)
}

fn agent_chain_with_sessions(config: &Config, sessions: bool) -> anyhow::Result<AgentChain> {
    let env = agent_env(config);
    let mut scrub = EnvScrub::default();
    for name in config.secret_names() {
        scrub = scrub.with_name(name);
    }
    let mut backends: Vec<Arc<dyn AgentBackend>> = Vec::new();
    for name in &config.agent.chain {
        let provider =
            okbot_agent::parse_provider(name).with_context(|| format!("unknown agent `{name}`"))?;
        let settings = config
            .agent
            .providers
            .get(name)
            .cloned()
            .unwrap_or_default();
        let backend = CliBackend::new(CliBackendConfig {
            provider,
            binary: settings.cli.as_ref().map(|path| config.path(path)),
            model: settings.model,
            scrub: scrub.clone(),
            env: env.clone(),
        })
        .with_context(|| format!("agent `{name}`"))?;
        let backend = if sessions {
            backend
        } else {
            backend.without_sessions()
        };
        backends.push(Arc::new(backend));
    }
    let mut chain = AgentChain::new(backends, config.agent.max_concurrent)
        .allow_unenforced_tools(config.agent.allow_unrestricted_agents);
    if let Some(slots) = machine_slots()? {
        chain = chain.machine_slots(slots);
    }
    if chain.caps().tool_policies.is_empty() && !config.agent.allow_unrestricted_agents {
        bail!(
            "none of the agents in [agent] chain can restrict their tools (only claude and codex \
             can); add one of them, or set allow_unrestricted_agents = true for a private bot"
        );
    }
    Ok(chain)
}

/// The runtime settings from `okbot.toml`.
pub(crate) fn runtime_config(config: &Config) -> anyhow::Result<RuntimeConfig> {
    let mut rc = RuntimeConfig::new(&config.bot.name, config.state_dir().join("work"));
    rc.agent_timeout = Duration::from_secs(config.agent.timeout_secs);
    rc.history_tokens = config.agent.history_tokens;
    rc.tool_policy = config.agent.tool_policy;
    rc.timezone.clone_from(&config.bot.timezone);
    rc.personal = config.bot.profile == okbot_config::BotProfile::Personal;
    rc.schedule.enabled = config.schedule.enabled;
    rc.schedule.late_grace = Duration::from_secs(config.schedule.late_grace_minutes * 60);
    rc.schedule.max_agent_runs_per_day = config.schedule.max_agent_runs_per_day;
    rc.schedule.deliver_to = config
        .schedule
        .deliver_to
        .as_deref()
        .map(str::parse)
        .transpose()?;
    for admin in &config.bot.admins {
        rc.access.admins.insert(parse_admin(admin)?);
    }
    for (id, person) in &config.people {
        let accounts = person
            .accounts
            .iter()
            .map(|a| okbot_config::parse_account(a).map_err(anyhow::Error::msg))
            .collect::<anyhow::Result<Vec<_>>>()?;
        rc.link_person(id, accounts);
    }
    let mut allow_users = HashSet::new();
    for (platform, instance, access) in config.channels.access() {
        let channel = ChannelId::new(platform, instance)?;
        rc.access
            .reply_on_by_channel
            .insert(channel.clone(), reply_on(access.reply_on));
        for user in access.allow_users {
            allow_users.insert((channel.clone(), UserId::new(user.as_str())));
        }
        for user in access.blocked_users {
            rc.access
                .blocked_users
                .insert((channel.clone(), UserId::new(user.as_str())));
        }
        for room in access.rooms {
            rc.rooms.insert(
                (channel.clone(), room.room.clone()),
                RoomRule {
                    tool_policy: room.tool_policy,
                    internal: room.internal,
                    personal: room.personal,
                },
            );
        }
    }
    if !allow_users.is_empty() {
        rc.access.allow_users = Some(allow_users);
    }
    for m in &config.mcp {
        rc.mcp.push(extra_mcp(config, m)?);
    }
    rc.commands = commands(config)?;
    for n in &config.notify {
        let token = config.secret(&n.token_env)?;
        crate::logging::redact(&token);
        let to =
            n.to.iter()
                .map(|t| t.parse())
                .collect::<Result<_, _>>()
                .with_context(|| format!("[[notify]] `{}`", n.name))?;
        rc.notify.push(okbot_runtime::NotifyRoute {
            name: n.name.clone(),
            token,
            to,
        });
    }
    rc.automations = automations(config)?;
    rc.roles = roles(config)?;
    rc.teams = config
        .teams
        .iter()
        .map(|(id, t)| okbot_runtime::TeamPolicy {
            id: id.clone(),
            roles: t.roles.clone().into_iter().collect(),
            delegate_roles: t.delegate_roles.clone(),
            confirm: t.confirm == okbot_config::Confirm::Always,
            wait: std::time::Duration::from_secs(t.wait),
            chain_tokens: (t.chain_tokens > 0).then_some(t.chain_tokens),
        })
        .collect();
    rc.limits = okbot_runtime::Limits {
        turns_per_day: config.limits.turns_per_day,
        tokens_per_day: config.limits.tokens_per_day,
    };
    Ok(rc)
}

/// One `[[mcp]]` server for the runtime, with its secrets (redacted from logs).
pub(crate) fn extra_mcp(
    config: &Config,
    m: &okbot_config::McpSection,
) -> anyhow::Result<okbot_runtime::ExtraMcp> {
    // The agent's variable for its gateway token, for servers whose token it never sees.
    let gateway_env = format!(
        "OKBOT_MCP_{}_TOKEN",
        m.name.to_ascii_uppercase().replace('-', "_")
    );
    let (token, token_env, stdio) = if m.auth == okbot_config::McpAuth::Oauth {
        (okbot_core::Secret::new(String::new()), gateway_env, None)
    } else if m.command.is_empty() {
        let token = config.secret(&m.token_env)?;
        (token, m.token_env.clone(), None)
    } else {
        let env = m
            .env
            .iter()
            .map(|name| {
                let value = config.secret(name)?;
                crate::logging::redact(&value);
                Ok((name.clone(), value))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let stdio = okbot_runtime::StdioServer {
            command: m.command.clone(),
            env,
        };
        (
            okbot_core::Secret::new(String::new()),
            gateway_env,
            Some(stdio),
        )
    };
    let oauth = if m.auth == okbot_config::McpAuth::Oauth {
        let settings = m.oauth.clone().unwrap_or_default();
        let client_secret = settings
            .client_secret_env
            .as_deref()
            .map(|name| config.secret(name))
            .transpose()?;
        if let Some(secret) = &client_secret {
            crate::logging::redact(secret);
        }
        Some(okbot_runtime::OAuthSettings {
            credentials: match m.credentials {
                okbot_config::McpCredentials::Person => okbot_runtime::Credentials::Person,
                okbot_config::McpCredentials::Shared => okbot_runtime::Credentials::Shared,
            },
            client_id: settings.client_id,
            client_secret,
            scopes: settings.scopes,
        })
    } else {
        None
    };
    if !token.expose().is_empty() {
        crate::logging::redact(&token);
    }
    Ok(okbot_runtime::ExtraMcp {
        server: okbot_core::McpServer {
            name: m.name.clone(),
            url: m.url.clone(),
            bearer_token_env: token_env,
        },
        token,
        admins_only: m.admins_only,
        signed: m.auth == okbot_config::McpAuth::Signed,
        rules: mcp_rules(m)?,
        stdio,
        oauth,
    })
}

/// `[[automation]]` for the runtime (ADR 0013).
pub(crate) fn automations(config: &Config) -> anyhow::Result<Vec<okbot_runtime::Automation>> {
    use okbot_runtime::{AutomationAction, RunAs, Trigger};
    config
        .automations
        .iter()
        .map(|a| {
            let what = || format!("[[automation]] `{}`", a.name);
            let run_as = config
                .automation_run_as(a)
                .map(|value| match value.strip_prefix("person:") {
                    Some(id) => Ok(RunAs::Person(id.to_string())),
                    None => okbot_config::parse_account(&value)
                        .map(|(channel, user)| RunAs::Account(channel, user))
                        .map_err(anyhow::Error::msg),
                })
                .transpose()
                .with_context(what)?;
            let trigger = if let Some(every) = &a.every {
                Trigger::Every(okbot_config::parse_every(every).map_err(anyhow::Error::msg)?)
            } else if let Some(rule) = &a.schedule {
                Trigger::Schedule(rule.parse().map_err(anyhow::Error::msg)?)
            } else {
                let token = config.secret(&a.token_env)?;
                crate::logging::redact(&token);
                Trigger::Webhook(token)
            };
            let source = a
                .source
                .as_ref()
                .map(|s| -> anyhow::Result<okbot_runtime::Source> {
                    let with = serde_json::to_value(&s.with).with_context(what)?;
                    Ok(okbot_runtime::Source {
                        mcp: s.mcp.clone(),
                        tool: s.tool.clone(),
                        with: with.as_object().cloned().unwrap_or_default(),
                        items: s.items.clone(),
                        id: s.id.clone(),
                    })
                })
                .transpose()?;
            let to =
                a.to.iter()
                    .map(|t| t.parse())
                    .collect::<Result<Vec<okbot_core::NotifyTo>, _>>()
                    .with_context(what)?;
            let (action, default_runs) = match (&a.text, &a.prompt) {
                (Some(template), _) => (
                    AutomationAction::Text {
                        template: template.clone(),
                        to,
                    },
                    200,
                ),
                (None, Some(prompt)) => (
                    AutomationAction::Prompt {
                        prompt: prompt.clone(),
                        to: to.into_iter().next(),
                    },
                    24,
                ),
                (None, None) => bail!("{}: no action", what()),
            };
            Ok(okbot_runtime::Automation {
                name: a.name.clone(),
                run_as,
                trigger,
                source,
                action,
                max_runs_per_day: a.max_runs_per_day.unwrap_or(default_runs),
                max_items: usize::try_from(a.max_items).unwrap_or(20),
                paused: a.paused,
            })
        })
        .collect()
}

/// `[roles]` for the runtime.
fn roles(config: &Config) -> anyhow::Result<okbot_runtime::Roles> {
    let mut roles = okbot_runtime::Roles::default();
    for (role, members) in &config.roles {
        // A role without members still exists: grants can fill it.
        roles.names.insert(role.clone());
        for member in members {
            let member = match member.strip_prefix("person:") {
                Some(id) => okbot_runtime::Member::Person(id.to_string()),
                None => {
                    let (channel, user) =
                        okbot_config::parse_account(member).map_err(anyhow::Error::msg)?;
                    okbot_runtime::Member::Account(channel, user)
                }
            };
            roles.add(role, member);
        }
    }
    Ok(roles)
}

/// The rules of one `[[mcp]]` server for the runtime.
pub(crate) fn mcp_rules(m: &okbot_config::McpSection) -> anyhow::Result<okbot_runtime::McpRules> {
    let rooms = m
        .rooms
        .iter()
        .map(|r| {
            let (channel, room) = okbot_config::parse_account(r).map_err(anyhow::Error::msg)?;
            Ok((channel, room.to_string()))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(okbot_runtime::McpRules {
        roles: m.roles.clone(),
        rooms,
        tools: m.tools.clone(),
        deny_tools: m.deny_tools.clone(),
        restrict: m
            .restrict
            .iter()
            .map(|r| okbot_runtime::Restrict {
                tools: r.tools.clone(),
                roles: r.roles.clone(),
            })
            .collect(),
        confirm: match m.confirm {
            okbot_config::Confirm::Never => okbot_runtime::Confirm::Never,
            okbot_config::Confirm::Destructive => okbot_runtime::Confirm::Destructive,
            okbot_config::Confirm::Always => okbot_runtime::Confirm::Always,
        },
        confirm_by: match m.confirm_by {
            okbot_config::ConfirmBy::Requester => okbot_runtime::ConfirmBy::Requester,
            okbot_config::ConfirmBy::Admins => okbot_runtime::ConfirmBy::Admins,
        },
    })
}

/// `[[command]]` for the runtime.
pub(crate) fn commands(config: &Config) -> anyhow::Result<Vec<okbot_runtime::McpCommand>> {
    config
        .commands
        .iter()
        .map(|c| {
            let with = serde_json::to_value(&c.with)
                .with_context(|| format!("[[command]] `{}`: with", c.name))?;
            Ok(okbot_runtime::McpCommand {
                name: c.name.clone(),
                description: c.description.clone(),
                descriptions: c
                    .descriptions
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
                mcp: c.mcp.clone(),
                tool: c.tool.clone(),
                args: c
                    .args
                    .iter()
                    .map(|a| okbot_runtime::CommandArg::parse(a))
                    .collect(),
                with: with.as_object().cloned().unwrap_or_default(),
                admins_only: c.admins_only,
                roles: c.roles.clone(),
            })
        })
        .collect()
}

#[cfg(feature = "zalo")]
fn zalo_channels(config: &Config) -> anyhow::Result<Vec<Arc<dyn Channel>>> {
    use okbot_channel_zalo::{DEFAULT_API_BASE, Delivery, ZaloChannel, ZaloConfig};
    let mut out: Vec<Arc<dyn Channel>> = Vec::new();
    for (instance, zalo) in &config.channels.zalo {
        let token = config.secret(&zalo.token_env)?;
        crate::logging::redact(&token);
        let delivery = match zalo.delivery {
            okbot_config::Delivery::Polling => Delivery::Polling {
                timeout: Duration::from_secs(30),
            },
            okbot_config::Delivery::Webhook => {
                let secret_env = zalo.secret_env.as_deref().unwrap_or_default();
                let secret = config.secret(secret_env)?;
                crate::logging::redact(&secret);
                Delivery::Webhook {
                    public_url: zalo.public_url.clone().unwrap_or_default(),
                    secret,
                }
            }
        };
        out.push(Arc::new(ZaloChannel::new(ZaloConfig {
            id: ChannelId::new("zalo", instance)?,
            token,
            delivery,
            api_base: std::env::var("OKBOT_ZALO_API_BASE")
                .unwrap_or_else(|_| DEFAULT_API_BASE.to_string()),
        })));
    }
    Ok(out)
}

#[cfg(not(feature = "zalo"))]
fn zalo_channels(config: &Config) -> anyhow::Result<Vec<Arc<dyn Channel>>> {
    if config.channels.zalo.is_empty() {
        Ok(Vec::new())
    } else {
        bail!("this okbot build has no Zalo support (feature `zalo`)")
    }
}

#[cfg(feature = "gchat")]
fn gchat_channels(config: &Config) -> anyhow::Result<Vec<Arc<dyn Channel>>> {
    use okbot_channel_gchat::{
        Audience, Delivery, Endpoints, GChatChannel, GChatConfig, ServiceAccount,
    };
    let mut out: Vec<Arc<dyn Channel>> = Vec::new();
    for (instance, g) in &config.channels.gchat {
        let path = config.path(&g.credentials);
        let account = ServiceAccount::from_file(&path)
            .with_context(|| format!("[channels.gchat.{instance}] credentials"))?;
        let delivery = match g.delivery {
            okbot_config::GChatDelivery::Pubsub => Delivery::PubSub {
                subscription: g.subscription.clone().unwrap_or_default(),
            },
            okbot_config::GChatDelivery::Http => Delivery::Http {
                audience: match (&g.audience_url, &g.audience_project_number) {
                    (Some(url), _) => Audience::EndpointUrl(url.clone()),
                    (None, Some(number)) => Audience::ProjectNumber(number.clone()),
                    (None, None) => bail!("[channels.gchat.{instance}] needs an audience"),
                },
            },
        };
        let mut endpoints = Endpoints::default();
        if let Ok(base) = std::env::var("OKBOT_GCHAT_API_BASE") {
            endpoints.chat.clone_from(&base);
            endpoints.pubsub.clone_from(&base);
            endpoints.token = Some(format!("{base}/token"));
            endpoints.jwks = Some(format!("{base}/jwks"));
        }
        out.push(Arc::new(GChatChannel::new(GChatConfig {
            id: ChannelId::new("gchat", instance)?,
            account,
            delivery,
            endpoints,
        })));
    }
    Ok(out)
}

#[cfg(not(feature = "gchat"))]
fn gchat_channels(config: &Config) -> anyhow::Result<Vec<Arc<dyn Channel>>> {
    if config.channels.gchat.is_empty() {
        Ok(Vec::new())
    } else {
        bail!("this okbot build has no Google Chat support (feature `gchat`)")
    }
}

#[cfg(feature = "telegram")]
fn telegram_channels(config: &Config) -> anyhow::Result<Vec<Arc<dyn Channel>>> {
    use okbot_channel_telegram::{API_BASE, Delivery, TelegramChannel, TelegramConfig};
    let mut out: Vec<Arc<dyn Channel>> = Vec::new();
    for (instance, t) in &config.channels.telegram {
        let token = config.secret(&t.token_env)?;
        crate::logging::redact(&token);
        let delivery = match t.delivery {
            okbot_config::Delivery::Polling => Delivery::Polling {
                timeout: Duration::from_secs(30),
            },
            okbot_config::Delivery::Webhook => {
                let secret_env = t.secret_env.as_deref().unwrap_or_default();
                let secret = config.secret(secret_env)?;
                if !okbot_channel_telegram::valid_secret(secret.expose()) {
                    bail!(
                        "{secret_env} must be 1–256 characters of A-Z, a-z, 0-9, `_` and `-` \
                         (a Telegram webhook secret)"
                    );
                }
                crate::logging::redact(&secret);
                Delivery::Webhook {
                    public_url: t.public_url.clone().unwrap_or_default(),
                    secret,
                }
            }
        };
        out.push(Arc::new(TelegramChannel::new(TelegramConfig {
            id: ChannelId::new("telegram", instance)?,
            token,
            delivery,
            api_base: std::env::var("OKBOT_TELEGRAM_API_BASE")
                .unwrap_or_else(|_| API_BASE.to_string()),
        })));
    }
    Ok(out)
}

#[cfg(not(feature = "telegram"))]
fn telegram_channels(config: &Config) -> anyhow::Result<Vec<Arc<dyn Channel>>> {
    if config.channels.telegram.is_empty() {
        Ok(Vec::new())
    } else {
        bail!("this okbot build has no Telegram support (feature `telegram`)")
    }
}

#[cfg(feature = "discord")]
fn discord_channels(config: &Config) -> anyhow::Result<Vec<Arc<dyn Channel>>> {
    use okbot_channel_discord::{DiscordChannel, DiscordConfig};
    let mut out: Vec<Arc<dyn Channel>> = Vec::new();
    for (instance, d) in &config.channels.discord {
        let token = config.secret(&d.token_env)?;
        crate::logging::redact(&token);
        out.push(Arc::new(DiscordChannel::new(DiscordConfig {
            id: ChannelId::new("discord", instance)?,
            token,
            message_content: d.message_content,
            http_proxy: std::env::var("OKBOT_DISCORD_HTTP_PROXY").ok(),
        })));
    }
    Ok(out)
}

#[cfg(not(feature = "discord"))]
fn discord_channels(config: &Config) -> anyhow::Result<Vec<Arc<dyn Channel>>> {
    if config.channels.discord.is_empty() {
        Ok(Vec::new())
    } else {
        bail!("this okbot build has no Discord support (feature `discord`)")
    }
}

#[cfg(feature = "slack")]
fn slack_channels(config: &Config) -> anyhow::Result<Vec<Arc<dyn Channel>>> {
    use okbot_channel_slack::{SLACK_API, SlackChannel, SlackConfig};
    let mut out: Vec<Arc<dyn Channel>> = Vec::new();
    for (instance, sl) in &config.channels.slack {
        let app_token = config.secret(&sl.app_token_env)?;
        let bot_token = config.secret(&sl.bot_token_env)?;
        crate::logging::redact(&app_token);
        crate::logging::redact(&bot_token);
        out.push(Arc::new(SlackChannel::new(SlackConfig {
            id: ChannelId::new("slack", instance)?,
            app_token,
            bot_token,
            api_base: std::env::var("OKBOT_SLACK_API_BASE").unwrap_or_else(|_| SLACK_API.into()),
        })));
    }
    Ok(out)
}

#[cfg(not(feature = "slack"))]
fn slack_channels(config: &Config) -> anyhow::Result<Vec<Arc<dyn Channel>>> {
    if config.channels.slack.is_empty() {
        Ok(Vec::new())
    } else {
        bail!("this okbot build has no Slack support (feature `slack`)")
    }
}

#[cfg(feature = "bridge")]
fn bridge_channels(config: &Config) -> anyhow::Result<Vec<Arc<dyn Channel>>> {
    use okbot_channel_bridge::{BridgeChannel, BridgeConfig};
    use okbot_core::{ChannelCaps, LengthUnit};
    let mut out: Vec<Arc<dyn Channel>> = Vec::new();
    for (instance, b) in &config.channels.bridge {
        let mut env = Vec::new();
        for name in &b.env {
            let value = config.secret(name)?;
            crate::logging::redact(&value);
            env.push((name.clone(), value));
        }
        out.push(Arc::new(BridgeChannel::new(BridgeConfig {
            id: ChannelId::new(&b.platform, instance)?,
            // A path is relative to the bot directory; a bare name is looked up on `PATH`.
            command: match &b.command {
                Some(c) if c.contains('/') => config.dir.join(c).display().to_string(),
                Some(c) => c.clone(),
                None => format!("okbot-channel-{}", b.platform),
            },
            args: b.args.clone(),
            env_remove: config
                .secret_names()
                .into_iter()
                .filter(|name| !b.env.contains(name))
                .collect(),
            env,
            settings: serde_json::to_value(&b.settings)?,
            caps: ChannelCaps {
                max_message_len: b.max_message_len,
                length_unit: LengthUnit::Chars,
                dialect: b.dialect,
                threads: b.threads,
                edit: b.edit,
                typing: b.typing,
            },
        })));
    }
    Ok(out)
}

#[cfg(not(feature = "bridge"))]
fn bridge_channels(config: &Config) -> anyhow::Result<Vec<Arc<dyn Channel>>> {
    if config.channels.bridge.is_empty() {
        Ok(Vec::new())
    } else {
        bail!("this okbot build has no bridge support (feature `bridge`)")
    }
}

/// Opens the knowledge bundle and starts its MCP endpoint.
pub(crate) async fn knowledge(
    config: &Config,
    stop: CancellationToken,
) -> anyhow::Result<Arc<Knowledge>> {
    let kb = config.kb_dir();
    if !kb.is_dir() {
        bail!(
            "the knowledge bundle {} does not exist; run `okbot init` in {}",
            kb.display(),
            config.dir.display()
        );
    }
    let mut options = KnowledgeOptions::new(&kb);
    options.index_dir = Some(config.state_dir().join("index"));
    options.timezone.clone_from(&config.bot.timezone);
    let knowledge = Arc::new(
        tokio::task::spawn_blocking(move || Knowledge::open(options))
            .await?
            .with_context(|| format!("cannot open the knowledge bundle {}", kb.display()))?,
    );
    knowledge.start_mcp(stop).await?;
    Ok(knowledge)
}

/// Builds the bot described by `config`.
pub(crate) async fn build(config: &Config, stop: CancellationToken) -> anyhow::Result<Bot> {
    let mut channels = zalo_channels(config)?;
    channels.extend(gchat_channels(config)?);
    channels.extend(discord_channels(config)?);
    channels.extend(slack_channels(config)?);
    channels.extend(telegram_channels(config)?);
    channels.extend(bridge_channels(config)?);
    if channels.is_empty() && config.teams.is_empty() {
        bail!(
            "no channel is configured in {}; add one with `okbot init` (or use `okbot chat`)",
            config.dir.join(okbot_config::CONFIG_FILE).display()
        );
    }
    let state = config.state_dir();
    std::fs::create_dir_all(&state)
        .with_context(|| format!("cannot create {}", state.display()))?;
    // Chat history and agent work directories: the bot's user only.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("cannot restrict {}", state.display()))?;
    }
    let store = Arc::new(
        SqliteStore::open(&state.join("okbot.db"))
            .with_context(|| format!("cannot open the database in {}", state.display()))?,
    );
    let chain = agent_chain(config)?;
    let knowledge = knowledge(config, stop).await?;
    let connector = crate::mcp::connector(config, store.clone())?;
    let mut builder = Runtime::builder(runtime_config(config)?)
        .store(store.clone())
        .agent(Arc::new(chain))
        .renderer(Arc::new(MarkdownRenderer))
        .knowledge(knowledge.clone() as Arc<dyn KnowledgeProvider>)
        .memory(knowledge.clone() as Arc<dyn okbot_core::MemoryStore>)
        .tasks(knowledge as Arc<dyn okbot_core::TaskStore>);
    let needs_http = config
        .channels
        .zalo
        .values()
        .any(|z| z.delivery == okbot_config::Delivery::Webhook)
        || config
            .channels
            .telegram
            .values()
            .any(|t| t.delivery == okbot_config::Delivery::Webhook)
        || config
            .channels
            .gchat
            .values()
            .any(|g| g.delivery == okbot_config::GChatDelivery::Http);
    match (&config.http.listen, needs_http) {
        (Some(listen), _) => {
            let addr: SocketAddr = listen
                .parse()
                .with_context(|| format!("[http] listen `{listen}` is not an address"))?;
            builder = builder.http(HttpOptions::new(addr));
        }
        (None, true) => bail!("webhook delivery needs [http] listen = \"127.0.0.1:<port>\""),
        // Signed MCP servers show prompts through POST /interact: a loopback port is enough.
        (None, false)
            if config
                .mcp
                .iter()
                .any(|m| m.auth == okbot_config::McpAuth::Signed) =>
        {
            builder = builder.http(HttpOptions::new(SocketAddr::from(([127, 0, 0, 1], 0))));
        }
        (None, false) => {}
    }
    if let Some(connector) = connector {
        builder = builder.oauth(connector);
    }
    if let Some(node) = crate::team::node(config, store.clone()).await? {
        channels.extend(node.channels());
        builder = builder.peers(Arc::new(node));
    }
    for channel in channels {
        builder = builder.channel(channel);
    }
    Ok(Bot {
        runtime: builder.build()?,
        store,
        retention_days: config.bot.retention_days,
    })
}

/// Deletes history older than the retention period, hourly, until `stop`.
pub(crate) async fn retention(store: Arc<SqliteStore>, days: u32, stop: CancellationToken) {
    let keep = jiff::SignedDuration::from_hours(24 * i64::from(days.max(1)));
    loop {
        if let Ok(cutoff) = jiff::Timestamp::now().checked_sub(keep) {
            match store.purge_before(cutoff).await {
                Ok(0) => {}
                Ok(n) => tracing::info!(rows = n, "retention: deleted old history"),
                Err(error) => tracing::warn!(%error, "retention failed"),
            }
        }
        tokio::select! {
            () = stop.cancelled() => return,
            () = tokio::time::sleep(Duration::from_secs(3600)) => {}
        }
    }
}
