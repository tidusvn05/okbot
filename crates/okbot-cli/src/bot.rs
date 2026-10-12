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
    let (channel, user) = value
        .split_once('/')
        .with_context(|| format!("admin `{value}` must look like `zalo:shop/<user id>`"))?;
    Ok((channel.parse()?, UserId::new(user)))
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
            binary: settings.cli,
            model: settings.model,
            scrub: scrub.clone(),
            env: env.clone(),
        })
        .with_context(|| format!("agent `{name}`"))?;
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
fn runtime_config(config: &Config) -> anyhow::Result<RuntimeConfig> {
    let mut rc = RuntimeConfig::new(&config.bot.name, config.state_dir().join("work"));
    rc.agent_timeout = Duration::from_secs(config.agent.timeout_secs);
    rc.history_tokens = config.agent.history_tokens;
    rc.tool_policy = config.agent.tool_policy;
    rc.timezone.clone_from(&config.bot.timezone);
    for admin in &config.bot.admins {
        rc.access.admins.insert(parse_admin(admin)?);
    }
    let mut allow_users = HashSet::new();
    for (instance, zalo) in &config.channels.zalo {
        let channel = ChannelId::new("zalo", instance)?;
        rc.access
            .reply_on_by_channel
            .insert(channel.clone(), reply_on(&zalo.reply_on));
        for user in &zalo.allow_users {
            allow_users.insert((channel.clone(), UserId::new(user.as_str())));
        }
        for user in &zalo.blocked_users {
            rc.access
                .blocked_users
                .insert((channel.clone(), UserId::new(user.as_str())));
        }
        for room in &zalo.rooms {
            rc.rooms.insert(
                (channel.clone(), room.room.clone()),
                RoomRule {
                    tool_policy: room.tool_policy,
                    internal: room.internal,
                },
            );
        }
    }
    if !allow_users.is_empty() {
        rc.access.allow_users = Some(allow_users);
    }
    Ok(rc)
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
    let channels = zalo_channels(config)?;
    if channels.is_empty() {
        bail!(
            "no channel is configured in {}; add one with `okbot init` (or use `okbot chat`)",
            config.dir.join(okbot_config::CONFIG_FILE).display()
        );
    }
    let state = config.state_dir();
    std::fs::create_dir_all(&state)
        .with_context(|| format!("cannot create {}", state.display()))?;
    let store = Arc::new(
        SqliteStore::open(&state.join("okbot.db"))
            .with_context(|| format!("cannot open the database in {}", state.display()))?,
    );
    let chain = agent_chain(config)?;
    let knowledge = knowledge(config, stop).await?;
    let mut builder = Runtime::builder(runtime_config(config)?)
        .store(store.clone())
        .agent(Arc::new(chain))
        .renderer(Arc::new(MarkdownRenderer))
        .knowledge(knowledge.clone() as Arc<dyn KnowledgeProvider>)
        .memory(knowledge as Arc<dyn okbot_core::MemoryStore>);
    let needs_http = config
        .channels
        .zalo
        .values()
        .any(|z| z.delivery == okbot_config::Delivery::Webhook);
    match (&config.http.listen, needs_http) {
        (Some(listen), _) => {
            let addr: SocketAddr = listen
                .parse()
                .with_context(|| format!("[http] listen `{listen}` is not an address"))?;
            builder = builder.http(HttpOptions::new(addr));
        }
        (None, true) => bail!("webhook delivery needs [http] listen = \"127.0.0.1:<port>\""),
        (None, false) => {}
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
