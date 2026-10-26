//! `okbot chat`: the console channel wired to the full pipeline.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, bail};
use okbot_agent::{AgentChain, CliBackend, CliBackendConfig, EnvScrub, ScriptedBackend, detect};
use okbot_channel_console::{ConsoleChannel, ConsoleOptions, Preset};
use okbot_core::{AgentBackend, CancellationToken, KnowledgeProvider};
use okbot_knowledge::{Knowledge, KnowledgeOptions};
use okbot_render::MarkdownRenderer;
use okbot_runtime::{HttpOptions, Runtime, RuntimeConfig};
use okbot_store::SqliteStore;

/// Arguments of `okbot chat`.
#[derive(Debug, clap::Args)]
pub(crate) struct ChatArgs {
    /// Agent CLIs to use, in fallback order (`claude,codex`). Default: the first logged-in one.
    /// `echo` answers every message with itself, without an agent CLI.
    #[arg(long, value_delimiter = ',')]
    agent: Vec<String>,
    /// Answer from the rules in this TOML file instead of an agent CLI (for scripts and CI;
    /// see docs/platforms/console.md).
    #[arg(long, conflicts_with_all = ["agent", "model"])]
    replies: Option<PathBuf>,
    /// Model for the first agent, in its CLI's format (`sonnet@high`).
    #[arg(long)]
    model: Option<String>,
    /// Where conversation history and agent working directories live.
    #[arg(long, default_value = "state")]
    state_dir: PathBuf,
    /// Start with an empty state in a temporary directory, removed afterwards (for scripts).
    #[arg(long, conflicts_with = "state_dir")]
    fresh: bool,
    /// Serve okbot's HTTP endpoints (`/notify/<name>`, `/interact`) on this address. Without it,
    /// a loopback port is opened only when a signed `[[mcp]]` server needs `/interact`.
    #[arg(long)]
    http: Option<SocketAddr>,
    /// Name the bot uses for itself (and `@<name>` mentions it in rooms). Default: the bot
    /// directory's name, or `okbot`.
    #[arg(long)]
    name: Option<String>,
    /// The okbase knowledge bundle (skipped when the directory does not exist).
    #[arg(long, default_value = "kb")]
    kb: PathBuf,
    /// Your name in the conversation.
    #[arg(long, default_value = "you")]
    user: String,
    /// Behave like a platform: its formatting, length limit, edits, typing and buttons
    /// (console, zalo, gchat, discord, slack, telegram).
    #[arg(long, default_value = "console")]
    like: Preset,
    /// Apply the bot directory's admins, roles, rooms, `[[mcp]]` rules and limits as `okbot run`
    /// does, to test them with `:as` and `:room`. Console accounts are `console:local/<user>`.
    #[arg(long)]
    strict: bool,
    /// Also run agents that cannot restrict their tools (Devin, OpenCode): they may use a shell.
    /// Use only for your own local chat.
    #[arg(long)]
    unsafe_tools: bool,
}

async fn agents(args: &ChatArgs) -> anyhow::Result<Vec<Arc<dyn AgentBackend>>> {
    if let Some(path) = &args.replies {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        let backend =
            ScriptedBackend::from_toml(&text).with_context(|| path.display().to_string())?;
        return Ok(vec![Arc::new(backend)]);
    }
    let names = if args.agent.is_empty() {
        let found = detect::detect(&detect::SearchPaths::from_env()).await;
        let ready: Vec<_> = found.iter().filter(|c| c.is_ready()).collect();
        let Some(first) = ready.first() else {
            bail!(
                "no logged-in agent CLI found (looked for claude, codex, opencode, devin); \
                 log in to one, or pass --agent"
            );
        };
        vec![okbot_agent::provider_name(first.provider).to_string()]
    } else {
        args.agent.clone()
    };
    let mut backends: Vec<Arc<dyn AgentBackend>> = Vec::new();
    for (i, name) in names.iter().enumerate() {
        if name == "echo" {
            backends.push(Arc::new(ScriptedBackend::echo()));
            continue;
        }
        let provider = okbot_agent::parse_provider(name).with_context(|| {
            format!("unknown agent `{name}` (use claude, codex, opencode, devin or echo)")
        })?;
        let backend = CliBackend::new(CliBackendConfig {
            provider,
            binary: None,
            model: if i == 0 { args.model.clone() } else { None },
            scrub: EnvScrub::default(),
            env: Vec::new(),
        })?;
        backends.push(Arc::new(backend));
    }
    Ok(backends)
}

/// Accounts connected for the bot (`/connect`, `okbot mcp login`), used in the console too.
async fn connector(
    bot: Option<&okbot_config::Config>,
) -> anyhow::Result<Option<Arc<okbot_runtime::oauth::Connector>>> {
    match bot {
        Some(bot)
            if bot
                .mcp
                .iter()
                .any(|m| m.auth == okbot_config::McpAuth::Oauth) =>
        {
            let store: Arc<dyn okbot_core::ConversationStore> =
                Arc::new(crate::owner::store(bot).await?);
            crate::mcp::connector(bot, store)
        }
        _ => Ok(None),
    }
}

/// The runtime configuration for `okbot chat`, from the bot directory `bot` when there is one.
fn runtime_config(
    args: &ChatArgs,
    name: &str,
    state_dir: &Path,
    bot: Option<okbot_config::Config>,
    timezone: Option<String>,
) -> anyhow::Result<RuntimeConfig> {
    let mut config = match &bot {
        Some(bot) if args.strict => {
            let mut config = crate::bot::runtime_config(bot)
                .context("--strict uses the bot directory's whole configuration")?;
            config.work_dir = state_dir.join("work");
            config
        }
        None if args.strict => bail!("--strict needs a bot directory (run it where okbot.toml is)"),
        _ => RuntimeConfig::new(name, state_dir.join("work")),
    };
    config.bot_name = name.to_string();
    config.access.rate_limit.messages = 0;
    config.timezone = timezone;
    // Only the terminal's own user is the owner, not the people `:as` writes as.
    config.console_owner = Some(okbot_core::UserId::new(args.user.as_str()));
    if let Some(bot) = bot.filter(|_| !args.strict) {
        config.personal = bot.bot.profile == okbot_config::BotProfile::Personal;
        // The terminal user owns the bot directory, so admins_only servers and every role and
        // room are theirs too; the tool lists and confirmations still apply.
        for m in &bot.mcp {
            let mut extra = crate::bot::extra_mcp(&bot, m)?;
            extra.admins_only = false;
            extra.rules.roles.clear();
            extra.rules.rooms.clear();
            extra.rules.restrict.clear();
            config.mcp.push(extra);
        }
        config.commands = crate::bot::commands(&bot)?;
        // The terminal user may delegate to the bot's teams (ADR 0014).
        config.teams = crate::bot::team_policies(&bot);
        for team in &mut config.teams {
            team.delegate_roles = vec![okbot_runtime::ROLE_EVERYONE.into()];
        }
        for (id, person) in &bot.people {
            let accounts = person
                .accounts
                .iter()
                .filter_map(|a| okbot_config::parse_account(a).ok())
                .collect();
            config.link_person(id, accounts);
        }
    }
    Ok(config)
}

/// The chat's database in `state_dir`.
fn open_store(state_dir: &Path) -> anyhow::Result<Arc<SqliteStore>> {
    let store = SqliteStore::open(&state_dir.join("okbot.db"))
        .with_context(|| format!("cannot open the database in {}", state_dir.display()))?;
    Ok(Arc::new(store))
}

/// The bot's teams, to delegate from the terminal. Their channels are not run: tasks for this bot
/// go to the bot itself (`okbot run`), whose socket stays its own.
async fn peers(
    bot: Option<&okbot_config::Config>,
    store: Arc<SqliteStore>,
) -> anyhow::Result<Option<okbot_peer::Node>> {
    match bot {
        Some(bot) => crate::team::node(bot, store).await,
        None => Ok(None),
    }
}

/// The knowledge bundle at `kb`, when there is one.
async fn knowledge(
    kb: &Path,
    state_dir: &Path,
    timezone: Option<&str>,
    stop: &CancellationToken,
) -> anyhow::Result<Option<Arc<Knowledge>>> {
    if !kb.is_dir() {
        tracing::warn!(kb = %kb.display(), "no knowledge bundle; answering without one");
        return Ok(None);
    }
    let mut options = KnowledgeOptions::new(kb);
    options.index_dir = Some(state_dir.join("index"));
    options.timezone = timezone.map(str::to_string);
    let knowledge = Arc::new(
        tokio::task::spawn_blocking(move || Knowledge::open(options))
            .await?
            .with_context(|| format!("cannot open the knowledge bundle {}", kb.display()))?,
    );
    knowledge.start_mcp(stop.clone()).await?;
    Ok(Some(knowledge))
}

/// Where to serve okbot's HTTP endpoints, if anywhere.
fn http_addr(args: &ChatArgs, bot: Option<&okbot_config::Config>) -> Option<SocketAddr> {
    let signed = bot.is_some_and(|b| {
        b.mcp
            .iter()
            .any(|m| m.auth == okbot_config::McpAuth::Signed)
    });
    // Signed MCP servers show prompts through POST /interact: a loopback port is enough.
    args.http
        .or_else(|| signed.then(|| SocketAddr::from(([127, 0, 0, 1], 0))))
}

/// The console: with line editing in an interactive terminal, plain lines otherwise (scripts).
#[cfg(all(feature = "line-editing", unix))]
fn console(options: ConsoleOptions, state_dir: &Path) -> anyhow::Result<ConsoleChannel> {
    use std::io::IsTerminal as _;
    if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        return ConsoleChannel::interactive(options, Some(state_dir.join("console_history")))
            .context("cannot set up the terminal");
    }
    Ok(ConsoleChannel::stdio_with(options))
}

/// The console, reading plain lines.
#[cfg(not(all(feature = "line-editing", unix)))]
#[allow(clippy::unnecessary_wraps)] // the same signature as with line editing
fn console(options: ConsoleOptions, _state_dir: &Path) -> anyhow::Result<ConsoleChannel> {
    Ok(ConsoleChannel::stdio_with(options))
}

/// Inside a bot directory, chat as that bot: its profile, time zone and linked people apply.
fn bot_dir_config(strict: bool) -> anyhow::Result<Option<okbot_config::Config>> {
    match okbot_config::Config::load(Path::new(".")) {
        Ok(bot) => Ok(Some(bot)),
        Err(e) if Path::new(okbot_config::CONFIG_FILE).exists() => {
            if strict {
                return Err(e).context("--strict needs the bot directory's configuration");
            }
            tracing::warn!(error = %e, "ignoring the bot directory's configuration");
            Ok(None)
        }
        Err(_) => Ok(None),
    }
}

pub(crate) async fn run(args: ChatArgs) -> anyhow::Result<()> {
    crate::logging::init("warn");
    let backends = agents(&args).await?;
    let chain = AgentChain::new(backends, 1).allow_unenforced_tools(args.unsafe_tools);
    if !args.unsafe_tools && chain.caps().tool_policies.is_empty() {
        bail!(
            "the selected agent CLI cannot restrict its tools (only Claude Code and Codex can), \
             so okbot refuses to run it by default. For a local chat with yourself, pass \
             --unsafe-tools."
        );
    }
    // Kept until the end: dropping it removes the directory.
    let fresh = args
        .fresh
        .then(|| tempfile::Builder::new().prefix("okbot-chat-").tempdir())
        .transpose()
        .context("cannot create a temporary state directory")?;
    let state_dir = fresh
        .as_ref()
        .map_or(args.state_dir.clone(), |d| d.path().to_path_buf());
    std::fs::create_dir_all(&state_dir)
        .with_context(|| format!("cannot create {}", state_dir.display()))?;
    let store = open_store(&state_dir)?;

    let stop = CancellationToken::new();
    let bot = bot_dir_config(args.strict)?;
    let timezone = bot.as_ref().and_then(|b| b.bot.timezone.clone());
    let name = args
        .name
        .clone()
        .or_else(|| bot.as_ref().map(|b| b.bot.name.clone()))
        .unwrap_or_else(|| "okbot".into());
    let knowledge = knowledge(&args.kb, &state_dir, timezone.as_deref(), &stop).await?;

    eprintln!(
        "Chatting with {} like {} (Ctrl-D to quit, /help for the bot's commands, :help for \
         the console's).",
        name,
        args.like.name()
    );
    let options = ConsoleOptions {
        user: args.user.clone(),
        bot_name: name.clone(),
        preset: args.like,
    };
    let console = Arc::new(console(options, &state_dir)?);
    let closed = console.closed();
    let http = http_addr(&args, bot.as_ref());
    let connector = connector(bot.as_ref()).await?;
    let peers = peers(bot.as_ref(), store.clone()).await?;
    let charts = bot.as_ref().map(crate::bot::charts).transpose()?.flatten();
    let config = runtime_config(&args, &name, &state_dir, bot, timezone)?;
    let mut builder = Runtime::builder(config)
        .store(store)
        .agent(Arc::new(chain))
        .renderer(Arc::new(MarkdownRenderer))
        .channel(console.clone());
    if let Some(charts) = charts {
        builder = builder.charts(charts);
    }
    if let Some(knowledge) = knowledge {
        builder = builder
            .knowledge(knowledge.clone() as Arc<dyn KnowledgeProvider>)
            .memory(knowledge.clone() as Arc<dyn okbot_core::MemoryStore>)
            .tasks(knowledge as Arc<dyn okbot_core::TaskStore>);
    }
    if let Some(addr) = http {
        builder = builder.http(HttpOptions::new(addr));
    }
    if let Some(connector) = connector {
        builder = builder.oauth(connector);
    }
    if let Some(peers) = peers {
        builder = builder.peers(Arc::new(peers));
    }
    let runtime = builder.build()?;
    let handle = runtime.handle();
    console.set_idle_wait({
        let handle = handle.clone();
        Arc::new(move || {
            let handle = handle.clone();
            Box::pin(async move { handle.wait_idle().await })
        })
    });
    let task = tokio::spawn(runtime.run(stop.clone()));

    tokio::select! {
        () = closed.cancelled() => handle.wait_idle().await,
        _ = tokio::signal::ctrl_c() => {}
    }
    stop.cancel();
    task.await.context("runtime task")??;
    match console.failures() {
        0 => Ok(()),
        n => bail!("{n} :expect check(s) failed"),
    }
}
