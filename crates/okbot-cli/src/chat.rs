//! `okbot chat`: the console channel wired to the full pipeline.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, bail};
use okbot_agent::{AgentChain, CliBackend, CliBackendConfig, EnvScrub, detect};
use okbot_channel_console::ConsoleChannel;
use okbot_core::{AgentBackend, CancellationToken, KnowledgeProvider};
use okbot_knowledge::{Knowledge, KnowledgeOptions};
use okbot_render::MarkdownRenderer;
use okbot_runtime::{Runtime, RuntimeConfig};
use okbot_store::SqliteStore;

/// Arguments of `okbot chat`.
#[derive(Debug, clap::Args)]
pub(crate) struct ChatArgs {
    /// Agent CLIs to use, in fallback order (`claude,codex`). Default: the first logged-in one.
    #[arg(long, value_delimiter = ',')]
    agent: Vec<String>,
    /// Model for the first agent, in its CLI's format (`sonnet@high`).
    #[arg(long)]
    model: Option<String>,
    /// Where conversation history and agent working directories live.
    #[arg(long, default_value = "state")]
    state_dir: PathBuf,
    /// Name the bot uses for itself.
    #[arg(long, default_value = "okbot")]
    name: String,
    /// The okbase knowledge bundle (skipped when the directory does not exist).
    #[arg(long, default_value = "kb")]
    kb: PathBuf,
    /// Your name in the conversation.
    #[arg(long, default_value = "you")]
    user: String,
    /// Also run agents that cannot restrict their tools (Devin, OpenCode): they may use a shell.
    /// Use only for your own local chat.
    #[arg(long)]
    unsafe_tools: bool,
}

async fn agents(args: &ChatArgs) -> anyhow::Result<Vec<Arc<dyn AgentBackend>>> {
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
        let provider = okbot_agent::parse_provider(name).with_context(|| {
            format!("unknown agent `{name}` (use claude, codex, opencode or devin)")
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
    std::fs::create_dir_all(&args.state_dir)
        .with_context(|| format!("cannot create {}", args.state_dir.display()))?;
    let store = SqliteStore::open(&args.state_dir.join("okbot.db"))
        .with_context(|| format!("cannot open the database in {}", args.state_dir.display()))?;

    let stop = CancellationToken::new();
    // Inside a bot directory, chat as that bot: its profile, time zone and linked people apply.
    let bot = okbot_config::Config::load(Path::new(".")).ok();
    let timezone = bot.as_ref().and_then(|b| b.bot.timezone.clone());
    let knowledge = if args.kb.is_dir() {
        let mut options = KnowledgeOptions::new(&args.kb);
        options.index_dir = Some(args.state_dir.join("index"));
        options.timezone.clone_from(&timezone);
        let knowledge = Arc::new(
            tokio::task::spawn_blocking(move || Knowledge::open(options))
                .await?
                .with_context(|| {
                    format!("cannot open the knowledge bundle {}", args.kb.display())
                })?,
        );
        knowledge.start_mcp(stop.clone()).await?;
        Some(knowledge)
    } else {
        tracing::warn!(kb = %args.kb.display(), "no knowledge bundle; answering without one");
        None
    };

    let console = Arc::new(ConsoleChannel::stdio(&args.user));
    let closed = console.closed();
    let mut config = RuntimeConfig::new(&args.name, args.state_dir.join("work"));
    config.access.rate_limit.messages = 0;
    config.timezone = timezone;
    if let Some(bot) = bot {
        config.personal = bot.bot.profile == okbot_config::BotProfile::Personal;
        // The terminal user owns the bot directory, so admins_only servers are theirs too.
        for m in &bot.mcp {
            config.mcp.push(okbot_runtime::ExtraMcp {
                server: okbot_core::McpServer {
                    name: m.name.clone(),
                    url: m.url.clone(),
                    bearer_token_env: m.token_env.clone(),
                },
                token: bot.secret(&m.token_env)?,
                admins_only: false,
                signed: m.auth == okbot_config::McpAuth::Signed,
            });
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
    let mut builder = Runtime::builder(config)
        .store(Arc::new(store))
        .agent(Arc::new(chain))
        .renderer(Arc::new(MarkdownRenderer))
        .channel(console);
    if let Some(knowledge) = knowledge {
        builder = builder
            .knowledge(knowledge.clone() as Arc<dyn KnowledgeProvider>)
            .memory(knowledge.clone() as Arc<dyn okbot_core::MemoryStore>)
            .tasks(knowledge as Arc<dyn okbot_core::TaskStore>);
    }
    let runtime = builder.build()?;
    let handle = runtime.handle();
    let task = tokio::spawn(runtime.run(stop.clone()));

    eprintln!(
        "Chatting with {} (Ctrl-D to quit, /help for commands).",
        args.name
    );
    tokio::select! {
        () = closed.cancelled() => handle.wait_idle().await,
        _ = tokio::signal::ctrl_c() => {}
    }
    stop.cancel();
    task.await.context("runtime task")??;
    Ok(())
}
